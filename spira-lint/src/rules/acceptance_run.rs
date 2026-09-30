//! `acceptance-run` — the structural invariants of `spira/acceptance-run.sh` and
//! `spira/acceptance-agent.sh` that are properties of their text, not of a run. Moved from
//! the grep checks of `spira/test-acceptance-run.sh`; the checks that execute the scripts
//! stay in that suite. Contract: DESIGN.md.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use regex::Regex;

use super::conf_key_registry::{conf_keys, CONF_SH};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct AcceptanceRun;

const NAME: &str = "acceptance-run";
pub const SCRIPT: &str = "spira/acceptance-run.sh";
pub const AGENT: &str = "spira/acceptance-agent.sh";

/// One textual expectation of a file.
pub enum Want {
    /// The fixed string appears.
    Has(&'static str, &'static str),
    /// The regex matches somewhere.
    HasRe(&'static str, &'static str),
    /// The regex matches nowhere.
    LacksRe(&'static str, &'static str),
}

use Want::*;

/// acceptance-run.sh's textual contract, in the order the suite asserted it.
pub const SCRIPT_WANTS: &[Want] = &[
    Has("sources acceptance-lib.sh", ". \"$HERE/acceptance-lib.sh\""),
    Has("emits a verdict line", "verdict:"),
    Has("verdict reports FAIL on any failure", "verdict=\"FAIL\""),
    Has("verdict reports PASS on zero failures", "verdict=\"PASS\""),
    Has("positive-control self-check present", "positive-control: command -v catches missing tool"),
    HasRe("landing is verified by ancestry over a SHA range", r"_base_sha_before\.\."),
    LacksRe("landing check does not rely on a bead_status shortcut", r"bead_status"),
    Has("phase A label present", "phase A"),
    Has("phase B label present", "phase B"),
    Has("phase C label present", "phase C"),
    Has("phase D label present", "phase D"),
    Has("--record writes a git note", "git notes --ref=acceptance"),
    Has("--record note targets refs/tags/", "refs/tags/"),
    HasRe("--waive-upgrade is a recognized flag", r"waive-upgrade\) do_waive_upgrade=1"),
    HasRe("waiver clears prev_tag regardless of --prev-tag", r#"do_waive_upgrade.*-eq 1.*&&.*prev_tag="""#),
    Has("verdict note records the waiver", "upgrade phases waived by operator"),
    LacksRe("old unbounded aeon-wait loop is gone", r"_aeon_wait"),
    LacksRe("old unbounded aged-land-wait loop is gone", r"_aged_land_wait"),
    Has("phase A stage 2 budget is configurable (summon window)", "_a_t2 )) -lt \"${SPIRA_ACCEPT_SUMMON_SECS:-180}\""),
    Has("phase D stage 2 budget is configurable (summon window)", "_d_t2 )) -lt \"${SPIRA_ACCEPT_SUMMON_SECS:-180}\""),
    LacksRe("sentinel start does not hardcode the base unit name", r"start spira-sentinel\.service"),
    Has("stage 5 budget is 120s", "_a_t5 )) -lt 120"),
    Has("phase A bead creation uses the plan-label variable", "\"acceptance,${_a_plan_label}"),
    Has("phase A bead creation uses the scope-label variable", "\"acceptance,${_a_plan_label},${_a_scope_label}"),
    Has("phase A claimability check uses bd ready", "bd -C \"$bd_db\" ready"),
    Has("claimability check filters by plan+scope label", "--label \"${_a_scope_label},${_a_plan_label}\""),
    Has("phase A bead-not-claimable error names the predicate", "builder predicate does not match bead labels"),
    LacksRe(
        "label read is not re-gated on a clean install (sp-bn9go)",
        r#"_install_rc" -eq 0 \] && \[ -f "\$_releases/current/spira/conf\.sh""#,
    ),
    Has("label read is gated only on the release conf existing (sp-bn9go)", "if [ -f \"$_releases/current/spira/conf.sh\" ]; then"),
    LacksRe("phase B install.sh output is not discarded", r#"env "\$\{_install_env\[@\]\}".*install\.sh.*>/dev/null"#),
    Has("phase A and B share one _phase_env-built _install_env", "_phase_env _install_env"),
    Has("phase B installs from the prev tarball via _install_from_tarball", "_install_from_tarball \"$_prev_tarball_file\""),
    HasRe("phase B deploys only when install succeeded", r"_prev_install_rc.*-ne 0"),
    Has("phase B guard names database service not started", "database service not started"),
    Has("phase B checks the .tag sidecar from the .tags dir", ".tags/"),
    Has("phase B checks SPIRA_PROD updated", "SPIRA_PROD"),
    Has("phase C captures the pre-upgrade unit set", "_units_pre_upgrade"),
    Has("phase C captures the post-rollback unit set", "_units_post_rollback"),
    Has("phase C diffs pre vs post", "_unit_diff"),
    Has("phase D checks the bead count is preserved", "_aged_pre_beads"),
    Has("phase D checks the memory count is preserved", "_aged_pre_mems"),
    Has("phase D runs doctor.sh", "_aged_doctor_rc"),
    Has("phase D checks the operator override survives", "_aged_override_got"),
    Has("phase A stages the release under test into the local release source", "_stage_release_source \"$_release_src\""),
    Has("spira.conf points SPIRA_RELEASE_REPO at it", "SPIRA_RELEASE_REPO = %s"),
    HasRe("phase B stages the predecessor too", r#"_stage_release_source "\$_release_src" "\$_prev_tarball_file""#),
    Has("phase D checks for failed units", "_aged_failed"),
    Has("phase D checks the world is not halted", "_aged_world_out"),
    Has("phase D rollback-refused names the migration", "migrat"),
    Has("aged-install (from, to) pair recorded in the git note", "aged-install from="),
    HasRe("--tarball is a recognized flag", r"--tarball\) *tarball_path="),
    Has("phase A: --tarball acquires via _acquire_tarball", "_acquire_tarball \"$tag\" \"$tarball_path\" \"\""),
    Has("phase A: the download path acquires via _acquire_tarball too", "_acquire_tarball \"$tag\" \"\" \"$_tarball_dir\""),
    Has("phase A: --tarball reports the download as skipped", "download skipped"),
    Has("_ci_deploy_env is built once via _ci_env", "_ci_deploy_env=(); _ci_env _ci_deploy_env \"$_conf\""),
];

/// acceptance-agent.sh's textual contract.
pub const AGENT_WANTS: &[Want] = &[
    Has("acceptance-agent.sh drains stdin", "cat >/dev/null"),
    Has("acceptance-agent.sh closes the bead", "close"),
];

fn re(p: &str) -> Regex {
    Regex::new(p).expect("static regex")
}

/// Every unmet expectation in `text`, as a message.
pub fn judge(text: &str, wants: &[Want]) -> Vec<String> {
    let mut out = Vec::new();
    for w in wants {
        match w {
            Has(d, s) if !text.contains(s) => out.push(format!("{d}: not found: {s}")),
            HasRe(d, p) if !re(p).is_match(text) => out.push(format!("{d}: pattern not found: {p}")),
            LacksRe(d, p) => {
                if let Some(m) = re(p).find(text) {
                    out.push(format!("{d}: still present: {}", m.as_str()));
                }
            }
            _ => {}
        }
    }
    out
}

/// The checks over acceptance-run.sh that relate lines to one another.
pub fn judge_lines(text: &str, conf_keys: &[String]) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();

    // Every sentinel start resolves the installed unit by its real (instance-suffixed) name.
    let starts = lines.iter().filter(|l| l.contains("systemctl --user start")).count();
    let resolved = lines.iter().filter(|l| l.contains("list-unit-files 'spira-sentinel*.service'")).count();
    if starts == 0 || starts != resolved {
        out.push(format!(
            "every sentinel start resolves the installed unit by its real name: {starts} start(s), only {resolved} resolved via list-unit-files"
        ));
    }

    // Claimability does not rely on sentinel --report polling (a comment may mention it).
    if lines.iter().any(|l| l.contains("sentinel --report") && !l.trim_start().starts_with('#')) {
        out.push("claimability does not rely on sentinel --report polling: found sentinel --report".into());
    }

    // The scope label is read as the installed services resolve it: no SPIRA_HOME_REPO forced
    // onto the read (the matching line and the two after it).
    for (i, l) in lines.iter().enumerate() {
        if l.contains("_a_scope_label=\"$(SPIRA_CONF=")
            && lines[i..(i + 3).min(lines.len())].iter().any(|s| s.contains("SPIRA_HOME_REPO"))
        {
            out.push(format!("phase A scope label is read as the installed services resolve it: SPIRA_HOME_REPO forced onto the read at line {}", i + 1));
        }
    }

    // The aged-install override is a key conf.sh honours.
    let key_re = re(r"printf .\\n([A-Z_][A-Z0-9_]*) = ");
    let aged_key = lines
        .iter()
        .filter(|l| l.contains(">> \"$_aged_conf\""))
        .find_map(|l| key_re.captures(l).map(|c| c[1].to_string()));
    match aged_key {
        None => out.push("phase D writes an operator override into spira.conf: no KEY = line appended to $_aged_conf".into()),
        Some(k) if !conf_keys.contains(&k) => {
            out.push(format!("the phase D override key is one conf.sh honours: {k} is not in SPIRA_CONF_KEYS"))
        }
        Some(_) => {}
    }

    // Every deploy of the release under test passes --allow-draft.
    let deploy_tag = re(r#"deploy\.sh"? .*"\$tag""#);
    for (i, l) in lines.iter().enumerate() {
        if deploy_tag.is_match(l) && !l.contains("--allow-draft") {
            out.push(format!("every deploy of the release under test passes --allow-draft: line {}: {}", i + 1, l.trim()));
        }
    }

    // deploy/uninstall/world/doctor run from this checkout only under _ci_deploy_env.
    // A call is the tool by bare name (sp-gypjk: found on the PATH _ci_env hands it) or the
    // older `bash "$HERE/<tool>.sh"` shape, at the start of a command.
    let bare = re(r#"(^\s*|\$\(|\|\s*|&&\s*|;\s*|" )(bash "\$HERE/)?(deploy|uninstall|world|doctor)\.sh"?(\s|$)"#);
    let wired = re(r#""\$\{_ci_deploy_env\[@\]\}" (bash "\$HERE/)?(deploy|uninstall|world|doctor)\.sh"?(\s|$)"#);
    let conf_read = re(r#"\. "\$1/conf\.sh""#);
    for (i, l) in lines.iter().enumerate() {
        if l.trim_start().starts_with('#') {
            continue;
        }
        if bare.is_match(l) && !l.contains("_ci_deploy_env") {
            out.push(format!("every deploy/uninstall/world/doctor call runs under _ci_deploy_env: line {}: {}", i + 1, l.trim()));
        }
        if conf_read.is_match(l) && !l.contains("_ci_deploy_env") {
            out.push(format!("every \"$1/conf.sh\" read runs under _ci_deploy_env: line {}: {}", i + 1, l.trim()));
        }
    }
    let n = lines.iter().filter(|l| wired.is_match(l)).count();
    if n < 11 {
        out.push(format!("at least 11 deploy/uninstall/world/doctor call sites are wired through _ci_deploy_env: found {n}"));
    }
    out
}

fn executable(tree: &Tree, rel: &str) -> bool {
    fs::metadata(tree.root.join(rel)).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

impl Rule for AcceptanceRun {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path == SCRIPT || e.path == AGENT
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let mut out = Vec::new();
        let mut push = |path: &str, message: String| out.push(Finding { rule: NAME, path: path.into(), line: None, message });
        for (path, wants) in [(SCRIPT, SCRIPT_WANTS), (AGENT, AGENT_WANTS)] {
            let Some(text) = tree.text_of(path) else {
                push(path, "missing".into());
                continue;
            };
            if !executable(tree, path) {
                push(path, "not executable".into());
            }
            for m in judge(&text, wants) {
                push(path, m);
            }
            if path == SCRIPT {
                let keys = tree.text_of(CONF_SH).and_then(|t| conf_keys(&t)).unwrap_or_default();
                for m in judge_lines(&text, &keys) {
                    push(path, m);
                }
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "acceptance-run.sh's phases are only exercised by a real acceptance run on a clean \
machine; these textual invariants are what the gate can hold it to. Each message names the \
invariant and the pattern (spira-lint/src/rules/acceptance_run.rs says why each exists)."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    /// A script that meets every expectation: each fixed string, a match for each wanted
    /// regex, and the line-relating checks satisfied.
    fn good_script() -> String {
        let mut s = String::from("#!/usr/bin/env bash\n");
        for w in SCRIPT_WANTS {
            if let Has(_, x) = w {
                s.push_str(x);
                s.push('\n');
            }
        }
        s.push_str("_base_sha_before..HEAD\n--waive-upgrade) do_waive_upgrade=1 ;;\n");
        s.push_str("[ \"$do_waive_upgrade\" -eq 1 ] && prev_tag=\"\"\n");
        s.push_str("[ \"$_prev_install_rc\" -ne 0 ]\n");
        s.push_str("_stage_release_source \"$_release_src\" \"$_prev_tarball_file\"\n");
        s.push_str("--tarball) tarball_path=\"$2\" ;;\n");
        s.push_str("u=$(systemctl --user list-unit-files 'spira-sentinel*.service'); systemctl --user start \"$u\"\n");
        s.push_str("printf '\\nSPIRA_MAX_AEONS = 3\\n' >> \"$_aged_conf\"\n");
        for _ in 0..11 {
            s.push_str("\"${_ci_deploy_env[@]}\" bash \"$HERE/deploy.sh\" --allow-draft \"$tag\"\n");
        }
        s
    }

    fn keys() -> Vec<String> {
        vec!["SPIRA_MAX_AEONS".to_string()]
    }

    #[test]
    fn a_conforming_script_passes() {
        let s = good_script();
        assert_eq!(judge(&s, SCRIPT_WANTS), Vec::<String>::new());
        assert_eq!(judge_lines(&s, &keys()), Vec::<String>::new());
    }

    #[test]
    fn a_planted_violation_of_each_kind_is_caught() {
        let good = good_script();
        let missing = good.replace("verdict=\"PASS\"", "");
        assert!(judge(&missing, SCRIPT_WANTS).iter().any(|m| m.contains("verdict reports PASS")));
        let shortcut = format!("{good}x=$(bead_status sp-1)\n");
        assert!(judge(&shortcut, SCRIPT_WANTS).iter().any(|m| m.starts_with("landing check does not rely")));
        let bare = format!("{good}bash \"$HERE/doctor.sh\"\n");
        assert!(judge_lines(&bare, &keys()).iter().any(|m| m.contains("runs under _ci_deploy_env: line")));
        // sp-gypjk: the tools go by bare name now; an unwired bare call is caught the same.
        let bare_name = format!("{good}  doctor.sh 2>&1\n");
        assert!(judge_lines(&bare_name, &keys()).iter().any(|m| m.contains("runs under _ci_deploy_env: line")));
        let wired_name = format!("{good}  env \"${{_ci_deploy_env[@]}}\" doctor.sh 2>&1\n");
        assert!(!judge_lines(&wired_name, &keys()).iter().any(|m| m.contains("runs under _ci_deploy_env: line")));
        let draft = format!("{good}\"${{_ci_deploy_env[@]}}\" bash \"$HERE/deploy.sh\" \"$tag\"\n");
        assert!(judge_lines(&draft, &keys()).iter().any(|m| m.contains("--allow-draft")));
        let unit = format!("{good}systemctl --user start spira-x.service\n");
        assert!(judge_lines(&unit, &keys()).iter().any(|m| m.contains("sentinel start")));
        assert!(judge_lines(&good, &[]).iter().any(|m| m.contains("SPIRA_MAX_AEONS is not in SPIRA_CONF_KEYS")));
        let report = format!("{good}sentinel --report\n# sentinel --report in a comment is fine\n");
        assert_eq!(judge_lines(&report, &keys()).len(), 1);
    }

    #[test]
    fn missing_and_non_executable_scripts_are_findings() {
        let t = TempDir::new("acc");
        t.write(SCRIPT, &good_script());
        t.write(CONF_SH, "SPIRA_CONF_KEYS=\"\nSPIRA_MAX_AEONS\n\"\n");
        fs::set_permissions(t.path().join(SCRIPT), fs::Permissions::from_mode(0o755)).unwrap();
        let tree = Tree::from_paths(t.path(), [SCRIPT, CONF_SH], std::iter::empty::<&str>());
        let got: Vec<String> = AcceptanceRun.check(&tree).unwrap().iter().map(|f| f.to_string()).collect();
        assert_eq!(got, vec!["acceptance-run: spira/acceptance-agent.sh: missing"]);
        t.write(AGENT, "cat >/dev/null\nbd close\n");
        fs::set_permissions(t.path().join(SCRIPT), fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(t.path().join(AGENT), fs::Permissions::from_mode(0o755)).unwrap();
        let tree = Tree::from_paths(t.path(), [SCRIPT, AGENT, CONF_SH], std::iter::empty::<&str>());
        let got: Vec<String> = AcceptanceRun.check(&tree).unwrap().iter().map(|f| f.to_string()).collect();
        assert_eq!(got, vec!["acceptance-run: spira/acceptance-run.sh: not executable"]);
    }
}
