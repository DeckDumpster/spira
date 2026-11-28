//! No instance a Round AMI run launched outlives the run: a step that always runs, on a fresh
//! role session, finds them by the run's tag and terminates them.

#[macro_use]
extern crate spira_test_id;
const WORKFLOW: &str = include_str!("../../.github/workflows/round-ami-build.yml");
const RUN_TAG_KEY: &str = "spira:round-build-run";
const BUILD: &str = "name: Build, image and tag";
const INSTANCES: &str = "name: Remove the build instances";
const STAGED: &str = "name: Remove the staged tree";

/// The job's steps, comment lines dropped and continued shell lines joined.
fn steps() -> Vec<String> {
    let code: Vec<&str> = WORKFLOW.lines().filter(|l| !l.trim_start().starts_with('#')).collect();
    let code = code.join("\n").replace("\\\n", " ");
    let (_, body) = code.split_once("\n    steps:\n").expect("the job has steps");
    format!("\n{body}").split("\n      - ").skip(1).map(str::to_string).collect()
}

fn step_named(steps: &[String], name: &str) -> usize {
    steps.iter().position(|s| s.lines().next() == Some(name)).unwrap_or_else(|| panic!("the workflow has no step {name:?}"))
}

fn always(step: &str) -> bool {
    step.lines().any(|l| l.trim() == "if: always() && steps.have.outputs.fresh != '1'")
}

#[test_id(t5hlfjp)]
#[test]
fn every_instance_is_launched_with_the_runs_tag() {
    let steps = steps();
    let build = &steps[step_named(&steps, BUILD)];
    let spec = build.lines().find(|l| l.contains("ResourceType=instance,Tags=")).expect("run-instances tags its instance");
    assert!(spec.contains(&format!("{{Key={RUN_TAG_KEY},Value=$RUN_TAG}}")), "the instance tag specification carries no {RUN_TAG_KEY}: {}", spec.trim());
    assert!(WORKFLOW.contains("\n      RUN_TAG: ${{ github.run_id }}-${{ github.run_attempt }}\n"), "RUN_TAG does not name the run and attempt");
}

#[test_id(ta9b6qa)]
#[test]
fn no_failed_termination_is_swallowed() {
    let steps = steps();
    let terminations: Vec<&str> = steps.iter().flat_map(|s| s.lines()).filter(|l| l.contains("terminate-instances")).collect();
    assert!(!terminations.is_empty(), "the matcher found no terminate-instances in the workflow");
    let swallowed: Vec<&&str> = terminations.iter().filter(|l| l.contains("|| true")).collect();
    assert!(swallowed.is_empty(), "a failed termination reads as success: {swallowed:?}");
}

#[test_id(t422e0l)]
#[test]
fn the_cleanup_steps_run_on_a_fresh_role_session() {
    let steps = steps();
    let build = step_named(&steps, BUILD);
    let sessions: Vec<usize> = (0..steps.len()).filter(|&i| steps[i].contains("aws-actions/configure-aws-credentials@v4")).collect();
    let first = &steps[*sessions.first().expect("the job takes a role session")];
    let fresh = *sessions.iter().find(|&&i| i > build && always(&steps[i])).unwrap_or_else(|| panic!("no always() configure-aws-credentials step follows {BUILD:?}"));
    for cleanup in [INSTANCES, STAGED] {
        assert!(fresh < step_named(&steps, cleanup), "{cleanup:?} runs before the fresh session");
        assert!(always(&steps[step_named(&steps, cleanup)]), "{cleanup:?} does not always run");
    }
    for key in ["role-to-assume:", "aws-region:"] {
        let of = |s: &str| s.lines().map(str::trim).find(|l| l.starts_with(key)).map(str::to_string).unwrap_or_else(|| panic!("a session step has no {key}"));
        assert_eq!(of(&steps[fresh]), of(first));
    }
}

#[test_id(tcv867e)]
#[test]
fn the_runs_instances_are_found_by_tag_terminated_and_awaited() {
    let steps = steps();
    let step = &steps[step_named(&steps, INSTANCES)];
    assert!(always(step), "{INSTANCES:?} does not always run");
    let at = |needle: &str| step.find(needle).unwrap_or_else(|| panic!("{INSTANCES:?} has no {needle:?}"));
    let found = at(&format!("--filters \"Name=tag:{RUN_TAG_KEY},Values=$RUN_TAG\""));
    let named = at("echo \"found build instance $id ($state)\"");
    let terminate = at("ec2 terminate-instances");
    let wait = at("ec2 wait instance-terminated");
    let done = at("echo \"no build instance left for run $RUN_TAG\"");
    assert!(found < named && named < terminate && terminate < wait && wait < done, "find, name, terminate, wait, report: {step}");
    assert!(!step.contains("|| true") && step.contains("set -euo pipefail"), "a failed call must fail the step: {step}");
    for named in ["spira:round-build-run=$RUN_TAG", "${doomed[*]}", "${unfinished[*]}"] {
        let handler = step.lines().find(|l| l.contains("|| { echo") && l.contains(named)).unwrap_or_else(|| panic!("no failure handler names {named}"));
        assert!(handler.ends_with(">&2; exit 1; }"), "the handler does not fail the step: {}", handler.trim());
    }
    for arm in [r#"pending|running|stopping|stopped) doomed+=("$id"); unfinished+=("$id") ;;"#, "terminated) ;;", r#"*) unfinished+=("$id") ;;"#] {
        assert!(step.lines().any(|l| l.trim() == arm), "no state arm {arm}");
    }
}

#[test_id(tul6xpl)]
#[test]
fn the_build_times_out_before_the_job_does() {
    let minutes = |text: &str, indent: &str| -> Option<u32> { text.lines().find_map(|l| l.strip_prefix(indent)?.strip_prefix("timeout-minutes: ")?.parse().ok()) };
    let job = minutes(WORKFLOW, "    ").expect("the job has a timeout");
    let steps = steps();
    let build = minutes(&steps[step_named(&steps, BUILD)], "        ").expect("the build step has no timeout of its own");
    assert!(job >= build + 20, "job {job} min leaves the cleanup steps no room after a {build} min build");
}
