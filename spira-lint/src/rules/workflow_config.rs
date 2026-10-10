//! `workflow-config` — a workflow step that runs a release binary has a resolvable
//! `SPIRA_TOML`: in its own `env:`, its job's, the workflow's, or written to `GITHUB_ENV` by
//! an earlier step of the same job. A hosted runner has no box config and the binary refuses
//! without one. Contract: DESIGN.md.

use std::collections::BTreeSet;

use regex::Regex;

use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct WorkflowConfig;

const NAME: &str = "workflow-config";
const KEY: &str = "SPIRA_TOML";

struct Step {
    title: String,
    line: usize,
    run: String,
    has_key: bool,
}

struct Job {
    steps: Vec<Step>,
    has_key: bool,
    stages_path: bool,
}

fn indent(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

fn env_has_key(lines: &[(usize, &str)], at: usize) -> bool {
    let base = indent(lines[at].1);
    lines[at + 1..]
        .iter()
        .take_while(|(_, l)| l.trim().is_empty() || indent(l) > base)
        .any(|(_, l)| l.trim_start().starts_with(&format!("{KEY}:")))
}

fn workflow_env_has_key(text: &str) -> bool {
    let ls: Vec<(usize, &str)> = text.lines().enumerate().map(|(i, l)| (i + 1, l)).collect();
    ls.iter().position(|(_, l)| *l == "env:").is_some_and(|i| env_has_key(&ls, i))
}

fn jobs(text: &str) -> Vec<Job> {
    let ls: Vec<(usize, &str)> = text.lines().enumerate().map(|(i, l)| (i + 1, l)).collect();
    let Some(start) = ls.iter().position(|(_, l)| *l == "jobs:") else { return Vec::new() };
    let mut out: Vec<Job> = Vec::new();
    let mut i = start + 1;
    while i < ls.len() {
        let (n, l) = ls[i];
        if l.is_empty() || l.trim_start().starts_with('#') {
            i += 1;
            continue;
        }
        if indent(l) == 0 {
            break;
        }
        if indent(l) == 2 && l.trim_end().ends_with(':') {
            out.push(Job { steps: Vec::new(), has_key: false, stages_path: false });
        } else if let Some(job) = out.last_mut() {
            if indent(l) == 4 && l.trim() == "env:" && env_has_key(&ls, i) {
                job.has_key = true;
            }
            if indent(l) == 6 && l.trim_start().starts_with("- ") {
                let title = l.trim_start().trim_start_matches("- ").trim_start_matches("name:").trim().to_string();
                job.steps.push(Step { title, line: n, run: String::new(), has_key: false });
            } else if let Some(step) = job.steps.last_mut() {
                let t = l.trim_start();
                if indent(l) == 8 && t == "env:" && env_has_key(&ls, i) {
                    step.has_key = true;
                }
                if indent(l) == 8 && t.starts_with("name:") && step.title.is_empty() {
                    step.title = t.trim_start_matches("name:").trim().to_string();
                }
                if indent(l) >= 8 && !t.starts_with('#') {
                    step.run.push_str(l);
                    step.run.push('\n');
                }
            }
        }
        i += 1;
    }
    for j in &mut out {
        j.stages_path = j.steps.iter().any(|s| s.run.contains("PATH=") && (s.run.contains("GITHUB_ENV") || s.run.contains("GITHUB_PATH")));
    }
    out
}

fn first_words(run: &str) -> Vec<String> {
    let split = Regex::new(r"&&|\|\||[;|(`\n]|\$\(").expect("static regex");
    let mut out = Vec::new();
    for seg in split.split(run) {
        let mut words = seg.split_whitespace().skip_while(|w| w.contains('=') && !w.starts_with('-') && !w.starts_with('"'));
        if let Some(w) = words.next() {
            out.push(w.trim_matches('"').to_string());
        }
    }
    out
}

fn runs_release_binary(run: &str, bins: &BTreeSet<String>, path_staged: bool) -> bool {
    let path_ref = Regex::new(r#"(target/release|RUNNER_TEMP"?/build)/[A-Za-z]"#).expect("static regex");
    let body: String = run.lines().filter(|l| !l.trim_start().starts_with('#')).collect::<Vec<_>>().join("\n");
    if path_ref.is_match(&body) {
        return true;
    }
    path_staged && first_words(&body).iter().any(|w| bins.contains(w))
}

pub fn judge(path: &str, text: &str, bins: &BTreeSet<String>) -> Vec<Finding> {
    let wf_key = workflow_env_has_key(text);
    let mut out = Vec::new();
    for job in jobs(text) {
        let mut written = false;
        let mut staged = false;
        for s in &job.steps {
            let covered = wf_key || job.has_key || s.has_key || written;
            if !covered && runs_release_binary(&s.run, bins, staged) {
                out.push(Finding {
                    rule: NAME,
                    path: path.to_string(),
                    line: Some(s.line),
                    message: format!("step {:?} runs a release binary with no {KEY} in its env, its job's, or GITHUB_ENV from an earlier step", s.title),
                });
            }
            if s.run.contains(KEY) && s.run.contains("GITHUB_ENV") || s.run.contains("ci-config.sh") {
                written = true;
            }
            if job.stages_path && s.run.contains("PATH=") && (s.run.contains("GITHUB_ENV") || s.run.contains("GITHUB_PATH")) {
                staged = true;
            }
        }
    }
    out
}

fn bin_crates(tree: &Tree) -> BTreeSet<String> {
    tree.entries
        .iter()
        .filter_map(|e| e.path.strip_suffix("/src/main.rs"))
        .filter(|c| !c.contains('/'))
        .map(str::to_string)
        .collect()
}

impl Rule for WorkflowConfig {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path.starts_with(".github/workflows/") && e.path.ends_with(".yml")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let bins = bin_crates(tree);
        let mut out = Vec::new();
        for e in files {
            if let Some(text) = tree.text_of(&e.path) {
                out.extend(judge(&e.path, &text, &bins));
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "A hosted runner has no box config. Run `bash spira/ci-config.sh \"$GITHUB_WORKSPACE\"` in the job \
before the step (it writes SPIRA_TOML to GITHUB_ENV), or set SPIRA_TOML in the step's env."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(WorkflowConfig)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bins() -> BTreeSet<String> {
        ["testenv", "release"].iter().map(|s| s.to_string()).collect()
    }

    const BARE: &str = "jobs:\n  a:\n    runs-on: x\n    steps:\n      - name: stage\n        run: |\n          \"$RUNNER_TEMP/build/release\" build\n";

    #[test]
    fn an_unconfigured_step_is_red() {
        let got = judge("w.yml", BARE, &bins());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].line, Some(5));
    }

    #[test]
    fn each_source_of_config_makes_it_green() {
        let step_env = BARE.replace("        run:", "        env:\n          SPIRA_TOML: /x\n        run:");
        let job_env = BARE.replace("    steps:", "    env:\n      SPIRA_TOML: /x\n    steps:");
        let wf_env = format!("env:\n  SPIRA_TOML: /x\n{BARE}");
        let earlier = BARE.replace("      - name: stage", "      - name: cfg\n        run: printf 'SPIRA_TOML=/x\\n' >> \"$GITHUB_ENV\"\n      - name: stage");
        let later = BARE.replace("      - name: stage", "      - name: stage").to_string()
            + "      - name: cfg\n        run: printf 'SPIRA_TOML=/x\\n' >> \"$GITHUB_ENV\"\n";
        for ok in [&step_env, &job_env, &wf_env, &earlier] {
            assert!(judge("w.yml", ok, &bins()).is_empty(), "{ok}");
        }
        assert_eq!(judge("w.yml", &later, &bins()).len(), 1, "config written after the step does not count");
    }

    #[test]
    fn a_bare_tool_counts_only_after_the_path_is_staged() {
        let wf = "jobs:\n  a:\n    steps:\n      - name: before\n        run: testenv container image\n      - name: stage\n        run: printf 'PATH=%s\\n' \"$p\" >> \"$GITHUB_ENV\"\n      - name: after\n        run: |\n          testenv container publish\n      - name: locate\n        run: PATH=\"$p\" command -v testenv\n";
        let got = judge("w.yml", wf, &bins());
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].message.contains("after"));
    }

    #[test]
    fn comments_and_path_additions_are_not_invocations() {
        let wf = "jobs:\n  a:\n    steps:\n      - name: build\n        run: |\n          # target/release/foo is run later\n          printf '%s\\n' \"$W/target/release\" >> \"$GITHUB_PATH\"\n          find target/release -maxdepth 1\n";
        assert!(judge("w.yml", wf, &bins()).is_empty());
    }
}
