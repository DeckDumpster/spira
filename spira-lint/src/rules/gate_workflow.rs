//! `gate-workflow` — the CI workflows run the real gate, and a release is cut only from a
//! gate that actually ran something. Ported from `spira/test-gate-workflow.sh`, whose
//! header gives the six properties this holds and why each one cannot be allowed to drift.
//! Block extraction mirrors that suite's awk exactly, so each check reads the same scope.
//! Contract: DESIGN.md.

use regex::Regex;

use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct GateWorkflow;

const NAME: &str = "gate-workflow";
pub const GATE: &str = ".github/workflows/gate.yml";
pub const RELEASE: &str = ".github/workflows/release.yml";
pub const ACCEPTANCE: &str = ".github/workflows/acceptance.yml";
pub const IMAGE: &str = ".github/workflows/testenv-image.yml";
pub const PROV_FIX: &str = "spira/test-fixtures/ephemeral-ci-v1/provision-action.yml";
pub const TEAR_FIX: &str = "spira/test-fixtures/ephemeral-ci-v1/teardown-action.yml";

fn is_key2(l: &str) -> bool {
    // ^  [a-z_-]+:$
    l.strip_prefix("  ")
        .and_then(|r| r.strip_suffix(':'))
        .is_some_and(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase() || b == b'_' || b == b'-'))
}

fn join(ls: &[&str]) -> String {
    let mut s = String::new();
    for l in ls {
        s.push_str(l);
        s.push('\n');
    }
    s
}

/// The lines after the first line equal to `  <name>:`, up to the next two-space key.
pub fn block(text: &str, name: &str) -> String {
    let head = format!("  {name}:");
    let ls: Vec<&str> = text.lines().collect();
    let Some(i) = ls.iter().position(|l| *l == head) else { return String::new() };
    let body: Vec<&str> = ls[i + 1..].iter().take_while(|l| !is_key2(l)).copied().collect();
    join(&body)
}

/// A job's `if:` value (the first `    if:` line of its block), with the key stripped.
pub fn job_if(text: &str, name: &str) -> String {
    block(text, name)
        .lines()
        .find_map(|l| l.strip_prefix("    if:").map(|r| r.trim_start_matches(' ').to_string()))
        .unwrap_or_default()
}

/// A step's lines: after the first `      - name: <title>` line (a prefix match unless
/// `exact`), up to the next `      - name:`.
pub fn step(text: &str, title: &str, exact: bool) -> String {
    let head = format!("      - name: {title}");
    let ls: Vec<&str> = text.lines().collect();
    let Some(i) = ls.iter().position(|l| if exact { *l == head } else { l.starts_with(&head) }) else {
        return String::new();
    };
    join(&ls[i + 1..].iter().take_while(|l| !l.starts_with("      - name:")).copied().collect::<Vec<_>>())
}

/// The lines after the first line satisfying `start`, up to (not including) `end`.
fn between(text: &str, start: impl Fn(&str) -> bool, end: impl Fn(&str) -> bool) -> String {
    let ls: Vec<&str> = text.lines().collect();
    let Some(i) = ls.iter().position(|l| start(l)) else { return String::new() };
    join(&ls[i + 1..].iter().take_while(|l| !end(l)).copied().collect::<Vec<_>>())
}

/// The top-level `concurrency:` block, through the next top-level key.
pub fn concurrency(text: &str) -> String {
    let ls: Vec<&str> = text.lines().collect();
    let Some(i) = ls.iter().position(|l| l.starts_with("concurrency:")) else { return String::new() };
    let mut out = vec![ls[i]];
    for l in &ls[i + 1..] {
        out.push(l);
        if l.starts_with(|c: char| c.is_ascii_lowercase()) && !l.starts_with("concurrency:") {
            break;
        }
    }
    join(&out)
}

/// An action.yml's required inputs.
pub fn required_inputs(action: &str) -> Vec<String> {
    let (mut in_inputs, mut name, mut out) = (false, String::new(), Vec::new());
    for l in action.lines() {
        if l.starts_with("inputs:") {
            in_inputs = true;
            continue;
        }
        if l.starts_with("outputs:") || l.starts_with("runs:") {
            in_inputs = false;
            continue;
        }
        if in_inputs {
            if let Some(w) = l.strip_prefix("  ").and_then(|r| r.strip_suffix(':')) {
                if w.starts_with(|c: char| c.is_ascii_lowercase())
                    && w.len() > 1
                    && w.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                {
                    name = w.to_string();
                }
            }
            if !name.is_empty() && l == "    required: true" {
                out.push(name.clone());
            }
        }
    }
    out
}

/// A workflow passes provision's vm-token to teardown.
pub fn vm_token_wired(text: &str) -> bool {
    text.contains("vm-token: ${{ steps.vm.outputs.vm-token }}")
        && text
            .find("vm-token:")
            .is_some_and(|i| text[i + "vm-token:".len()..].contains("needs.provision.outputs.vm-token"))
}

/// A job that runs on the provisioned runner must not bypass a failed provision: its `if:`
/// either is absent, checks `needs.provision.result == 'success'`, or uses neither
/// `always()` nor `cancelled()`.
pub fn provision_if_ok(cond: &str) -> bool {
    cond.is_empty()
        || cond.contains("needs.provision.result == 'success'")
        || !(cond.contains("always()") || cond.contains("cancelled()"))
}

/// Jobs whose own `runs-on:` names the provisioned label.
pub fn provisioned_jobs(text: &str) -> Vec<String> {
    let (mut job, mut out) = (String::new(), Vec::new());
    for l in text.lines() {
        if is_key2(l) {
            job = l.trim().trim_end_matches(':').to_string();
        }
        if l.starts_with("    runs-on:") && l.contains("needs.provision.outputs.label") {
            out.push(job.clone());
        }
    }
    out
}

/// The workflow files, as read (`None` = absent).
pub struct Workflows {
    pub gate: Option<String>,
    pub release: Option<String>,
    pub acceptance: Option<String>,
    pub image: Option<String>,
    pub prov_fix: Option<String>,
    pub tear_fix: Option<String>,
}

/// The findings so far, as (file, message).
struct Judge(Vec<(&'static str, String)>);

impl Judge {
    fn fail(&mut self, file: &'static str, m: String) {
        self.0.push((file, m));
    }
    fn want(&mut self, file: &'static str, d: &str, needle: &str, hay: &str) {
        if !hay.contains(needle) {
            self.fail(file, format!("{d}: expected {needle:?}"));
        }
    }
    fn nowant(&mut self, file: &'static str, d: &str, needle: &str, hay: &str) {
        if hay.contains(needle) {
            self.fail(file, format!("{d}: {needle:?} is present"));
        }
    }
    /// A block the checks below read came back empty: they would be vacuous.
    fn located(&mut self, file: &'static str, what: &str, b: &str) {
        if b.is_empty() {
            self.fail(file, format!("{what} was not located (positive control) — the checks on it would be vacuous"));
        }
    }
}

/// Every unmet property, as (file, message).
pub fn judge(w: &Workflows) -> Vec<(&'static str, String)> {
    let mut j = Judge(Vec::new());
    let Some(g) = w.gate.as_deref() else {
        j.fail(GATE, "missing".into());
        return j.0;
    };

    // 1. the real gate, not a cheaper subset
    j.want(GATE, "the file has a name (positive control)", "name:", g);
    j.want(GATE, "runs spira-lint's inventory rule", "spira-lint --only inventory", g);
    j.want(GATE, "runs spira-lint's literal-lint rule", "spira-lint --only literal-lint", g);
    j.want(GATE, "runs spira-lint's scratch-fence rule", "spira-lint --only scratch-fence", g);
    j.want(GATE, "runs the testenv runner on the staged release's build", "testenv --artifacts \"$SPIRA_RELEASE/bin\" --suites -", g);
    j.want(GATE, "its retry tests the same prebuilt set", "GATE_RETRY_ARTIFACTS=\"$SPIRA_RELEASE/bin\" bash spira/gate-retry.sh", g);
    // 2. queue PRs use diff-selected suites
    j.want(GATE, "the selected list is piped via --suites", "--suites", g);
    j.want(GATE, "queue PRs use the selector", "suite-select select", g);
    j.want(GATE, "queue PRs match spira/queue/", "spira/queue/", g);
    // 3. an infrastructure fault is not a branch failure
    j.want(GATE, "the harness-fault exit code is handled", "75", g);
    // 4. release only by explicit dispatch, only on the base branch
    let cut_if = job_if(g, "cut");
    let pub_if = job_if(g, "publish");
    j.located(GATE, "the cut job's if: condition", &cut_if);
    j.want(GATE, "a job depends on the gate", "needs:", g);
    j.want(GATE, "release.sh cut is what tags", "release.sh", g);
    j.want(GATE, "cut is restricted to the base branch", "refs/heads/main", &cut_if);
    j.want(GATE, "cut requires an explicit workflow_dispatch", "github.event_name == 'workflow_dispatch'", &cut_if);
    j.want(GATE, "cut requires the cut input to be true", "inputs.cut == true", &cut_if);
    // 4b. cut and publish survive skipped suites (sp-7k4qh)
    j.want(GATE, "cut names a status function", "!cancelled()", &cut_if);
    j.want(GATE, "cut requires the gate to have passed", "needs.gate.result == 'success'", &cut_if);
    j.want(GATE, "publish names a status function", "!cancelled()", &pub_if);
    j.want(GATE, "publish requires cut to have passed", "needs.cut.result == 'success'", &pub_if);
    // 4c. cut sources its positive control from the run that tested the SHA (sp-5h238)
    let cut_step = {
        let ls: Vec<&str> = g.lines().collect();
        match ls.iter().position(|l| *l == "  cut:") {
            Some(i) => join(&ls[i..].iter().take_while(|l| **l != "  publish:").copied().collect::<Vec<_>>()),
            None => String::new(),
        }
    };
    j.want(GATE, "cut walks the runs for an artifact", "for _rid in", &cut_step);
    // 5. publishing does not rely on the tag trigger
    j.want(GATE, "release.yml is invoked directly", "uses: ./.github/workflows/release.yml", g);
    match w.release.as_deref() {
        Some(r) => {
            j.want(RELEASE, "release.yml accepts being called", "workflow_call", r);
            // 24. the draft release is pinned to the built commit (sp-j7t1e)
            let rb = step(r, "Create GitHub release (draft)", true);
            j.located(RELEASE, "the create-draft step", &rb);
            j.want(RELEASE, "release.yml resolves the checked-out commit", "git rev-parse HEAD", &rb);
            j.want(RELEASE, "release.yml passes --target to gh release create", "--target", &rb);
        }
        None => j.fail(RELEASE, "missing".into()),
    }
    // 6. history and tags are fetched
    j.want(GATE, "fetch-depth is set", "fetch-depth", g);
    j.want(GATE, "tags are fetched", "fetch-tags", g);
    // 7. the runner is provisioned per run
    j.want(GATE, "the VM is provisioned", "ephemeral-ci/provision@v1", g);
    j.want(GATE, "the gate targets that VM", "needs.provision.outputs.label", g);
    // 8. the VM is destroyed whatever the outcome
    j.want(GATE, "teardown runs", "ephemeral-ci/teardown@v1", g);
    j.want(GATE, "teardown is unconditional", "always()", g);
    // 9. the gate confirms which machine it landed on
    j.want(GATE, "the runner identity is checked", "RUNNER_NAME", g);
    // 10. host dependencies are installed
    j.want(GATE, "host dependencies are installed", "runner-deps.sh", g);
    // 11. the test image is acquired, not built
    j.want(GATE, "the gate names a registry", "SPIRA_TESTENV_REGISTRY", g);
    j.want(GATE, "the gate authenticates to it", "podman login", g);
    // 12. the image is published only under the closure hash
    match w.image.as_deref() {
        Some(i) => {
            j.want(IMAGE, "the file has a name (positive control)", "name:", i);
            j.want(IMAGE, "it publishes through testenv's container driver", "testenv container publish", i);
            j.want(IMAGE, "an already-published tag is skipped", "manifest inspect", i);
            j.want(IMAGE, "the registry path is lowercased", "tr '[:upper:]' '[:lower:]'", i);
        }
        None => j.fail(IMAGE, "missing".into()),
    }
    // 13. two pushes to the base branch cannot cancel one another
    let conc = concurrency(g);
    j.want(GATE, "the concurrency block was found (positive control)", "group:", &conc);
    j.want(GATE, "the push group is keyed on the commit", "github.sha", &conc);
    j.want(GATE, "a pull request still supersedes itself", "cancel-in-progress", &conc);
    // 13b. a closed PR still enters the gate-ref group (sp-1p04d)
    let pr = block(g, "pull_request");
    j.located(GATE, "the pull_request trigger block", &pr);
    j.want(GATE, "the pull_request trigger adds the closed type", "closed", &pr);
    j.want(GATE, "the pull_request trigger keeps opened", "opened", &pr);
    j.want(GATE, "the group still keys every pull_request run on the ref", "github.ref", &conc);
    // 13c. a closed-PR run does no work
    let sel_job = block(g, "select");
    j.located(GATE, "the select job block", &sel_job);
    j.want(GATE, "select refuses to run for a closed PR", "event.action != 'closed'", &sel_job);
    // the suites job is bounded
    let suites_job = block(g, "suites");
    j.located(GATE, "the suites job block", &suites_job);
    let tm = Regex::new(r"(?m)^[ \t]*timeout-minutes:[ \t]*([0-9]+)").expect("static regex");
    match tm.captures(&suites_job).and_then(|c| c[1].parse::<u64>().ok()) {
        None => j.fail(GATE, "the suites job sets no timeout-minutes: it inherits GitHub's 360-minute default and a wedge holds a provisioned VM for six hours".into()),
        Some(t) if !(t > 24 && t < 360) => {
            j.fail(GATE, format!("the suites timeout-minutes is {t}: it must exceed one measured pass (24) and stay under the 360 default"))
        }
        Some(_) => {}
    }
    // 14. push to main selects nothing
    let sel = step(g, "Select suites", false);
    j.located(GATE, "the Select suites step", &sel);
    j.want(GATE, "select matches queue PRs", "spira/queue/", &sel);
    j.want(GATE, "queue selection uses the selector", "suite-select select", &sel);
    j.want(GATE, "PR selection uses the selector's gate pipeline", "suite-select gate", &sel);
    j.want(GATE, "select has a dedicated push branch", "\"push\"", &sel);
    let push_branch = between(&sel, |l| l.contains("= \"push\""), |l| l == "          else");
    j.located(GATE, "the push branch of the select step", &push_branch);
    j.want(GATE, "push selects nothing", "_s=\"\"", &push_branch);
    let fallback = between(&sel, |l| l == "          else", |l| l == "          fi");
    j.located(GATE, "the merge_group/dispatch fallback branch", &fallback);
    j.want(GATE, "the merge_group/dispatch fallback enumerates the corpus", "test-*.sh", &fallback);
    let suites_step = step(g, "Suites", false);
    j.located(GATE, "the Suites step block", &suites_step);
    j.want(GATE, "the suites step pipes the pre-computed list to the runner", "--suites -", &suites_step);
    // 15. cut asserts a green gate check and non-empty suite results before tagging
    let cut = block(g, "cut");
    j.located(GATE, "the cut job block", &cut);
    for (d, n) in [
        ("cut confirms the SHA is a queue PR head", "spira/queue/*"),
        ("cut queries check-runs for the SHA", "check-runs"),
        ("cut filters on the gate check name", "\"gate\""),
        ("cut looks up the queue PR's own run", "event=pull_request"),
        ("cut checks the batch-results artifact", "batch-results"),
        ("cut counts .result files", ".result"),
        ("cut has actions:read to read the queue PR's run", "actions: read"),
        ("release.sh cut receives the workspace", "release.sh cut"),
        // 18. acceptance fires via an App-token tag push
        ("cut mints an App token", "app-tok"),
        ("cut reads the App private key", "SPIRA_GH_APP_PRIVATE_KEY"),
        ("cut's tag push uses the App token", "app-tok.outputs.token"),
        ("cut's tag push clears the checkout-persisted credential", "extraheader"),
        ("cut's tag push routes through a credential helper", "credential.helper"),
        // 23. cut refuses the job when the tag push fails (sp-j7t1e)
        ("cut checks the tag push for failure", "push origin \"refs/tags/$tag\" || {"),
        ("a failed tag push exits the job non-zero", "exit 1"),
    ] {
        j.want(GATE, d, n, &cut);
    }
    let cut_out = between(&cut, |l| l.starts_with("    outputs:"), |l| {
        l.strip_prefix("    ").is_some_and(|r| {
            r.split_once(':').is_some_and(|(k, _)| !k.is_empty() && k.bytes().all(|b| b.is_ascii_lowercase() || b == b'_' || b == b'-'))
        })
    });
    j.want(GATE, "cut outputs prev-tag", "prev-tag", &cut_out);
    // 16. a failed provision leaves the gate check failing, not skipped
    let gate_job = block(g, "gate");
    j.located(GATE, "the gate verdict job block", &gate_job);
    for (d, n) in [
        ("gate needs provision", "provision"),
        ("gate needs suites", "suites"),
        ("gate needs build", "build"),
        ("gate runs even when needs failed", "!cancelled()"),
        ("gate runs on a hosted runner", "ubuntu-latest"),
        ("gate exits 75 on a provision fault", "75"),
        ("gate checks provision.result", "provision.result"),
        ("gate checks suites.result", "suites.result"),
        // 19. an empty selection is green
        ("gate exits cleanly on an empty selection", "exit 0"),
        ("gate checks the select output before provision", "select.outputs.suites"),
    ] {
        j.want(GATE, d, n, &gate_job);
    }
    // 16b. build runs in parallel with provision; suites downloads its binaries
    let build = block(g, "build");
    j.located(GATE, "the build job block", &build);
    j.want(GATE, "the build job needs select", "needs: select", &build);
    j.want(GATE, "the build job calls make build", "make build", &build);
    j.want(GATE, "the build job uploads the binaries artifact", "upload-artifact", &build);
    j.want(GATE, "the suites job needs build", "build", &suites_job);
    j.want(GATE, "the suites job downloads the binaries artifact", "download-artifact", &suites_job);
    // 29. GitHub Actions is a launcher: the suites job stages its build as a release and
    // sets SPIRA_RELEASE and PATH from it for every later step (sp-6cbna). Without it
    // conf.sh fails closed on a missing spira-config and every by-name tool resolves to
    // nothing of this commit's.
    let stage = step(g, "Stage the build as a release", true);
    j.located(GATE, "the Stage the build as a release step", &stage);
    j.want(GATE, "the staging step lays out a release from the tested build", "/release\" build", &stage);
    j.want(GATE, "the staging step takes the downloaded binaries, running no cargo", "--bin-dir", &stage);
    j.want(GATE, "the staging step sets SPIRA_RELEASE for every later step", "SPIRA_RELEASE=%s", &stage);
    j.want(GATE, "the staging step sets PATH for every later step", "PATH=%s", &stage);
    j.want(GATE, "the staging step names the checkout as the repository under test", "SPIRA_REPO=%s", &stage);
    j.want(GATE, "the staging step writes both to GITHUB_ENV", "GITHUB_ENV", &stage);
    j.want(GATE, "PATH starts with the staged release", "_path=\"$_rel/bin:$_rel/spira:", &stage);
    j.want(GATE, "the suites step runs after the staging step", "Stage the build as a release", &suites_job);
    // 30. lints never scan build output: the binaries land outside the checkout, and the
    // lints run spira-lint by name from the release, never a bin/ inside the tree.
    let dl = step(g, "Download binaries", true);
    j.located(GATE, "the Download binaries step", &dl);
    j.want(GATE, "binaries download outside the checkout", "path: ${{ runner.temp }}/", &dl);
    let lints = step(g, "Lints", true);
    j.located(GATE, "the Lints step", &lints);
    j.want(GATE, "the lints assert spira-lint resolves from the staged release", "\"$SPIRA_RELEASE/bin/spira-lint\"", &lints);
    j.nowant(GATE, "the lints do not run a spira-lint inside the checkout", "bin/spira-lint --only", &lints);
    // 19/20. provision is conditional on a selection and waits for the guest agent
    let prov = block(g, "provision");
    j.located(GATE, "the provision job block", &prov);
    j.want(GATE, "provision needs the select job", "select", &prov);
    j.want(GATE, "provision is conditional on suite selection", "needs.select.outputs.suites", &prov);
    j.want(GATE, "provision sets AGENT_TIMEOUT", "AGENT_TIMEOUT", &prov);
    j.want(GATE, "provision raises it to 300", "AGENT_TIMEOUT: 300", &prov);
    // 22/22b. attribution can dispatch a run scoped to suites; cut is a boolean input
    let wd = block(g, "workflow_dispatch");
    j.located(GATE, "the workflow_dispatch block", &wd);
    for (d, n) in [
        ("workflow_dispatch accepts a suites input", "suites:"),
        ("the suites input is optional", "required: false"),
        ("workflow_dispatch accepts a cut input", "cut:"),
        ("the cut input is a boolean", "type: boolean"),
        ("the cut input defaults to false", "default: false"),
    ] {
        j.want(GATE, d, n, &wd);
    }
    j.want(GATE, "select checks for workflow_dispatch", "workflow_dispatch", &sel);
    j.want(GATE, "select reads the suites input", "inputs.suites", &sel);
    j.want(GATE, "select checks the cut input", "inputs.cut", &sel);
    // 17. every required action input at the pinned v1 is passed
    let (prov_req, tear_req) = match (w.prov_fix.as_deref(), w.tear_fix.as_deref()) {
        (Some(p), Some(t)) => (required_inputs(p), required_inputs(t)),
        _ => {
            j.fail(PROV_FIX, "the ephemeral-ci v1 action fixtures (provision, teardown) are missing".into());
            (Vec::new(), Vec::new())
        }
    };
    let check_inputs = |j: &mut Judge, file: &'static str, text: &str| {
        for (job, req) in [("provision", &prov_req), ("teardown", &tear_req)] {
            let b = block(text, job);
            j.located(file, &format!("the {job} job block"), &b);
            for inp in req.iter() {
                j.want(file, &format!("{job} passes required action input {inp}"), inp, &b);
            }
        }
    };
    check_inputs(&mut j, GATE, g);
    // 21. teardown receives provision's vm-token; 28. no job bypasses a failed provision
    let provisioned = |file: &'static str, text: &str| {
        let mut o = Vec::new();
        if !vm_token_wired(text) {
            o.push((file, "provision must output vm-token and teardown must receive needs.provision.outputs.vm-token".to_string()));
        }
        let pb = block(text, "provision");
        if pb.is_empty() {
            o.push((file, "the provision job was located (positive control): not found".to_string()));
        } else if !pb.contains("AGENT_TIMEOUT: 300") {
            o.push((file, "provision sets AGENT_TIMEOUT to 300: expected \"AGENT_TIMEOUT: 300\"".to_string()));
        }
        for job in provisioned_jobs(text) {
            let cond = job_if(text, &job);
            if !provision_if_ok(&cond) {
                o.push((file, format!(
                    "job '{job}' runs on the provisioned runner but its if: [{cond}] uses always()/cancelled() without checking needs.provision.result — it queues on a label a failed provision never registers"
                )));
            }
        }
        o
    };
    let mut late = provisioned(GATE, g);
    match w.acceptance.as_deref() {
        Some(a) => {
            check_inputs(&mut j, ACCEPTANCE, a);
            late.extend(provisioned(ACCEPTANCE, a));
            for (d, n) in [
                ("acceptance.yml was read (positive control)", "workflow_dispatch"),
                ("acceptance.yml accepts a tag input", "tag:"),
                ("acceptance.yml accepts a prev-tag input", "prev-tag:"),
                // 25. the tag must agree with the tested MANIFEST (sp-j7t1e)
                ("acceptance.yml verifies the tag before publishing", "Verify the tag is bound to the tested commit"),
                ("the verify step resolves the tag to a commit", "git rev-parse \"${_tag}^{commit}\""),
                ("the verify step reads the release asset's MANIFEST", "release download"),
                ("the verify step compares against the MANIFEST commit", "$_manifest_commit\" = \"$_tag_commit\""),
                // 27. a re-run against an already-published tag is skipped
                ("provision is gated on the guard output", "needs.guard.outputs.already-published"),
                ("acceptance is gated on the guard output", "needs: [provision, guard]"),
                ("teardown is gated on the guard output", "needs: [provision, acceptance, guard]"),
            ] {
                j.want(ACCEPTANCE, d, n, a);
            }
            // 26. never retract an already-published release
            let rs = step(a, "Retract release and tag on FAIL", true);
            j.located(ACCEPTANCE, "the retract step", &rs);
            j.want(ACCEPTANCE, "retract checks isDraft before deleting", "isDraft", &rs);
            j.want(ACCEPTANCE, "retract refuses on an already-published release", "$_draft\" != \"true\"", &rs);
            let retract_if = rs.lines().find_map(|l| l.trim().strip_prefix("if:")).unwrap_or("");
            j.want(ACCEPTANCE, "retract also fires on a cancelled run", "cancelled()", retract_if);
            j.want(ACCEPTANCE, "retract still fires on a failed run", "failure()", retract_if);
            let gj = block(a, "guard");
            j.located(ACCEPTANCE, "the guard job", &gj);
            j.want(ACCEPTANCE, "guard runs only on a tag push", "github.event_name == 'push'", &gj);
            j.want(ACCEPTANCE, "guard checks whether the release already published", "isDraft", &gj);
        }
        None => j.fail(ACCEPTANCE, "missing".into()),
    }
    // The absences: pushes cannot cut, no fixed runner label, no floating image tag.
    j.nowant(GATE, "a push event cannot reach cut", "github.event_name == 'push'", &cut_if);
    j.nowant(GATE, "cut does not take the newest run blindly",
        "select(.name == \"Gate\" and .conclusion == \"success\")][0].id", &cut_step);
    j.nowant(GATE, "no fixed runner label", "self-hosted, linux, x64", g);
    j.nowant(GATE, "push does not enumerate the corpus", "test-*.sh", &push_branch);
    j.nowant(GATE, "the build job does not need provision", "provision", &build);
    if let Some(i) = w.image.as_deref() {
        j.nowant(IMAGE, "no floating tag is published", ":latest", i);
    }
    if let Some(a) = w.acceptance.as_deref() {
        j.nowant(ACCEPTANCE, "the comment no longer claims push auto-tests every release",
            "automatically tests each release", a);
    }
    j.0.extend(late);
    j.0
}

impl Rule for GateWorkflow {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        [GATE, RELEASE, ACCEPTANCE, IMAGE, PROV_FIX, TEAR_FIX].contains(&e.path.as_str())
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let w = Workflows {
            gate: tree.text_of(GATE),
            release: tree.text_of(RELEASE),
            acceptance: tree.text_of(ACCEPTANCE),
            image: tree.text_of(IMAGE),
            prov_fix: tree.text_of(PROV_FIX),
            tear_fix: tree.text_of(TEAR_FIX),
        };
        Ok(judge(&w)
            .into_iter()
            .map(|(path, message)| Finding { rule: NAME, path: path.into(), line: None, message })
            .collect())
    }

    fn hint(&self) -> &'static str {
        "A workflow runs on a machine nobody here owns and reports its own success. Each message \
names the property that drifted; spira-lint/src/rules/gate_workflow.rs and the workflow's own \
comments say why each one is load-bearing."
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    // The shipped workflows are the passing fixture; each test plants one violation in a
    // copy and requires the finding that names it.
    fn shipped() -> Workflows {
        Workflows {
            gate: Some(include_str!("../../../.github/workflows/gate.yml").to_string()),
            release: Some(include_str!("../../../.github/workflows/release.yml").to_string()),
            acceptance: Some(include_str!("../../../.github/workflows/acceptance.yml").to_string()),
            image: Some(include_str!("../../../.github/workflows/testenv-image.yml").to_string()),
            prov_fix: Some(include_str!("../../../spira/test-fixtures/ephemeral-ci-v1/provision-action.yml").to_string()),
            tear_fix: Some(include_str!("../../../spira/test-fixtures/ephemeral-ci-v1/teardown-action.yml").to_string()),
        }
    }

    fn planted(field: fn(&mut Workflows) -> &mut Option<String>, from: &str, to: &str) -> Vec<String> {
        let mut w = shipped();
        let f = field(&mut w);
        let before = f.clone().unwrap();
        let after = before.replacen(from, to, 1);
        assert_ne!(before, after, "the plant {from:?} no longer exists in the shipped file — update this test");
        *f = Some(after);
        judge(&w).into_iter().map(|(p, m)| format!("{p}: {m}")).collect()
    }

    fn gate(w: &mut Workflows) -> &mut Option<String> {
        &mut w.gate
    }
    fn acc(w: &mut Workflows) -> &mut Option<String> {
        &mut w.acceptance
    }

    #[test]
    fn the_shipped_workflows_pass() {
        assert_eq!(judge(&shipped()), Vec::<(&str, String)>::new());
    }

    #[test]
    fn a_push_that_can_reach_cut_is_caught() {
        let got = planted(gate, "inputs.cut == true", "inputs.cut == true || github.event_name == 'push'");
        assert!(got.iter().any(|m| m.contains("a push event cannot reach cut")), "{got:?}");
    }

    #[test]
    fn a_fixed_runner_label_is_caught() {
        let got = planted(gate, "name:", "runs-on: [self-hosted, linux, x64]\nname:");
        assert!(got.iter().any(|m| m.contains("no fixed runner label")), "{got:?}");
    }

    #[test]
    fn an_unbounded_suites_job_is_caught() {
        let t = Regex::new(r"(?m)^([ \t]*)timeout-minutes:[ \t]*[0-9]+").unwrap();
        let mut w = shipped();
        let g = w.gate.clone().unwrap();
        let s = block(&g, "suites");
        let unbounded = t.replace(&s, "${1}timeout-minutes: 400").into_owned();
        w.gate = Some(g.replacen(&s, &unbounded, 1));
        let got: Vec<String> = judge(&w).into_iter().map(|(_, m)| m).collect();
        assert!(got.iter().any(|m| m.contains("timeout-minutes is 400")), "{got:?}");
    }

    #[test]
    fn a_dropped_required_action_input_is_caught() {
        let mut w = shipped();
        let g = w.gate.clone().unwrap();
        let tear = block(&g, "teardown");
        let stripped = join(&tear.lines().filter(|l| !l.contains("pve-ca-cert")).collect::<Vec<_>>());
        assert_ne!(tear, stripped, "teardown no longer passes pve-ca-cert — update this test");
        w.gate = Some(g.replacen(&tear, &stripped, 1));
        let got: Vec<String> = judge(&w).into_iter().map(|(_, m)| m).collect();
        assert!(got.iter().any(|m| m.contains("teardown passes required action input pve-ca-cert")), "{got:?}");
    }

    #[test]
    fn a_job_that_bypasses_a_failed_provision_is_caught() {
        assert!(!provision_if_ok("always() && needs.guard.outputs.already-published != 'true'"));
        assert!(provision_if_ok("${{ !cancelled() && needs.provision.result == 'success' }}"));
        assert!(provision_if_ok(""));
        let got = planted(acc, "!cancelled() && needs.provision.result == 'success' &&", "always() &&");
        assert!(got.iter().any(|m| m.contains("runs on the provisioned runner")), "{got:?}");
    }

    #[test]
    fn a_retract_that_ignores_cancellation_is_caught() {
        let got = planted(acc, "if: ${{ failure() || cancelled() }}", "if: failure()");
        assert!(got.iter().any(|m| m.contains("retract also fires on a cancelled run")), "{got:?}");
    }

    #[test]
    fn an_unwired_vm_token_is_caught() {
        let mut w = shipped();
        let g = w.gate.clone().unwrap();
        w.gate = Some(join(&g.lines().filter(|l| !l.contains("vm-token")).collect::<Vec<_>>()));
        let got: Vec<String> = judge(&w).into_iter().map(|(_, m)| m).collect();
        assert!(got.iter().any(|m| m.contains("teardown must receive needs.provision.outputs.vm-token")), "{got:?}");
    }

    #[test]
    fn a_suites_job_that_never_stages_a_release_is_caught() {
        let got = planted(gate, "      - name: Stage the build as a release", "      - name: Build the release somewhere else");
        assert!(got.iter().any(|m| m.contains("the Stage the build as a release step was not located")), "{got:?}");
        let got = planted(gate, "printf 'PATH=%s\\n' \"$_path\" >> \"$GITHUB_ENV\"", "true");
        assert!(got.iter().any(|m| m.contains("the staging step sets PATH for every later step")), "{got:?}");
        let got = planted(gate, "printf 'SPIRA_REPO=%s\\n' \"$GITHUB_WORKSPACE\" >> \"$GITHUB_ENV\"", "true");
        assert!(got.iter().any(|m| m.contains("names the checkout as the repository under test")), "{got:?}");
    }

    #[test]
    fn build_output_inside_the_checkout_is_caught() {
        let got = planted(gate, "path: ${{ runner.temp }}/build", "path: bin/");
        assert!(got.iter().any(|m| m.contains("binaries download outside the checkout")), "{got:?}");
        let got = planted(gate, "          spira-lint --only inventory", "          bin/spira-lint --only inventory");
        assert!(got.iter().any(|m| m.contains("do not run a spira-lint inside the checkout")), "{got:?}");
    }

    #[test]
    fn a_missing_gate_workflow_is_a_finding() {
        let mut w = shipped();
        w.gate = None;
        assert_eq!(judge(&w), vec![(GATE, "missing".to_string())]);
    }

    #[test]
    fn block_extraction_matches_the_suites_awk() {
        let t = "on:\n  push:\n    branches: [main]\n  pull_request:\n    types: [closed]\njobs:\n  cut:\n    if: x\n    outputs:\n      prev-tag: y\n    steps:\n      - name: A\n        run: a\n      - name: B\n  publish:\n    if: z\n";
        assert_eq!(block(t, "pull_request"), "    types: [closed]\njobs:\n");
        assert_eq!(job_if(t, "cut"), "x");
        assert_eq!(step(t, "A", true), "        run: a\n");
        assert_eq!(concurrency("concurrency:\n  group: g\njobs:\n  x:\n"), "concurrency:\n  group: g\njobs:\n");
        assert_eq!(
            required_inputs("inputs:\n  vm-token:\n    required: false\n  pve-ca-cert:\n    required: true\nruns:\n  x:\n    required: true\n"),
            vec!["pve-ca-cert"]
        );
    }
}
