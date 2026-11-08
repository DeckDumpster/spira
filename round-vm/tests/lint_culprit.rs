use std::path::Path;
use std::process::Command;

fn function(name: &str) -> String {
    let s = round_vm::run::REMOTE_SCRIPT;
    let f = s.find(&format!("{name}() {{")).unwrap();
    s[f..f + s[f..].find("\n}\n").unwrap() + 3].to_string()
}

fn sh(dir: &Path, home: &Path, script: &str) -> String {
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .current_dir(dir)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "{script}\n{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn rule_src(name: &str, allow: &str) -> String {
    format!("const NAME: &str = \"{name}\";\nconst ALLOW_FILE: &str = \"{allow}\";\n")
}

#[test]
fn a_finding_whose_rule_arrives_in_a_members_diff_is_that_members_not_the_bases() {
    let tmp = testkit::TempDir::new("lint-culprit");
    let home = tmp.path().join("home");
    let tree = tmp.path().join("tree");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(tree.join("spira-lint/src/rules")).unwrap();
    std::fs::create_dir_all(tree.join("spira")).unwrap();
    std::fs::write(tree.join("spira/old.sh"), "state=$(grep open status.txt)\n").unwrap();
    std::fs::write(tree.join("spira/other.sh"), "true\n").unwrap();
    std::fs::write(tree.join("spira-lint/src/rules/old_rule.rs"), rule_src("old-rule", "spira-lint/old-rule-allow")).unwrap();
    std::fs::write(tree.join("spira-lint/old-rule-allow"), "").unwrap();
    std::fs::write(tree.join("spira-lint/src/rules/allow_rule.rs"), rule_src("allow-rule", "spira-lint/allow-rule-allow")).unwrap();
    std::fs::write(tree.join("spira-lint/allow-rule-allow"), "spira/old.sh\n").unwrap();
    let member = |id: &str, subject: &str, change: &str| {
        format!(
            "git checkout -q -b m-{id} base && {change} && git add -A && git commit -qm 'work {id}' && \
             git checkout -q round && git merge -q --no-ff -m '{subject}' m-{id}\n"
        )
    };
    let setup = format!(
        "set -euo pipefail\ngit init -q -b round . && git add -A && git commit -qm base && git branch base && \
         git update-ref refs/remotes/origin/base base\n{}{}{}",
        member(
            "sp-rule",
            "spira: land sp-rule — adds the state-from-text rule",
            "printf 'const NAME: &str = \"state-from-text\";\\nconst ALLOW_FILE: &str = \"spira-lint/state-from-text-allow\";\\n' > spira-lint/src/rules/state_from_text.rs && printf 'spira/new.sh\\n' > spira-lint/state-from-text-allow"
        ),
        member("sp-other", "spira: land sp-other", "echo false > spira/other.sh"),
        member("sp-allow", "round: merge sp-allow (spira/sp-allow)", ": > spira-lint/allow-rule-allow"),
    );
    sh(&tree, &home, &setup);
    let culprit = |finding: &str| {
        sh(&tree, &home, &format!("set -euo pipefail\n{}\nlint_culprit {:?} {:?}\n", function("lint_culprit"), tree.to_str().unwrap(), finding))
    };

    assert_eq!(culprit("state-from-text: spira/old.sh:1: state read from text"), "sp-rule", "the member that added the rule and an allow list missing an existing site");
    assert_eq!(culprit("allow-rule: spira/old.sh:1: not allowed"), "sp-allow", "the member that emptied an existing rule's allow list");
    assert_eq!(culprit("state-from-text: spira/other.sh:1: state read from text"), "sp-other", "a member touching the flagged file outranks the rule's author");
    let base = culprit("old-rule: spira/old.sh:1: old hit");
    assert!(base.starts_with("the base itself"), "positive control: a rule and file no member touched is the base's: {base}");
}
