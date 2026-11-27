//! The ops sweep's pieces as a library, for the scheduler's `persona.ops` task: the rune read
//! in-process, the brief rendered to its two files, the session's argv, the ledger lines and
//! the cleanup. `Run::sweep` does the same steps in one process with the session as its
//! child; the scheduler does each as an effect and runs the session as a transient unit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::brief;
use crate::conf::{self, Rune, SystemPrompt};
use crate::ledger::{self, Ledger};

/// What `persona.ops.brief` hands the machine: the dwarf's name, the two brief files, the
/// session's log and the argv and environment the transient unit runs with.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SweepBrief {
    pub name: String,
    pub sys_file: PathBuf,
    pub task_file: PathBuf,
    #[serde(default)]
    pub log: PathBuf,
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

/// What the brief needs from the pinned config.
#[derive(Clone, Debug, Default)]
pub struct BriefCfg {
    pub agent: String,
    pub memories_cache: String,
    pub memories_cache_age: i64,
    pub statute_core: String,
    pub statute_core_local: String,
    pub prod: String,
    /// The path `SPIRA_TOML` names: where `persona.<name>.model` is declared.
    pub toml: String,
}

/// Marks the refusal a missing core statute raises, so the machine can tell it from the rest.
pub const STATUTE_MISSING: &str = "core statute(s) declared but not found";

/// `<home>/runes/<name>.rune`, parsed in-process; `lookup` resolves the config it names.
pub fn rune(home: &Path, name: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<Rune, String> {
    let f = spira_config::runes::runes_dir(home).join(format!("{name}.rune"));
    let text = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
    let vars: BTreeMap<String, String> = spira_config::rune_file::parse(&f.to_string_lossy(), &text, lookup)?.into_iter().collect();
    Ok(Rune::from_vars(name, &vars))
}

/// The statutes in force for `rune`: the memories cache when fresh, else `memories` (the JSON
/// `bd memories --json` prints), with the core refused when a slug names no memory.
pub fn statutes(rune: &Rune, cfg: &BriefCfg, release: &Path, now: i64, memories: &dyn Fn() -> Result<String, String>) -> Result<String, String> {
    let cache = Path::new(&cfg.memories_cache);
    let mut json = String::new();
    if !cfg.memories_cache.is_empty() && cache.is_file() && now - crate::session::mtime(cache) < cfg.memories_cache_age {
        json = std::fs::read_to_string(cache).unwrap_or_default();
    }
    if json.trim().is_empty() {
        json = memories()?;
        if !cfg.memories_cache.is_empty() && !json.trim().is_empty() {
            if let Some(p) = cache.parent() {
                let _ = std::fs::create_dir_all(p);
            }
            let _ = std::fs::write(cache, format!("{}\n", json.trim_end_matches('\n')));
        }
    }
    let core = if rune.statute_core.is_empty() { cfg.statute_core.clone() } else { rune.statute_core.clone() };
    let core = brief::with_local_core(&core, &cfg.statute_core_local);
    let missing = brief::missing_core(&json, &rune.memory_prefixes, &core);
    if !missing.is_empty() {
        return Err(format!("{}: {STATUTE_MISSING}: {} — refusing to start with a thinned brief", rune.name, missing.join(", ")));
    }
    Ok(brief::render_memories(&json, &rune.memory_prefixes, 120_000, &core, &release.display().to_string()))
}

/// `Run::claude_argv` for a sweep: the agent's flags, without the program.
pub fn claude_args(home: &Path, run: &Path, prod: &str, rune: &Rune, sys_file: &Path, toml: &str) -> Result<Vec<String>, String> {
    let flag = match rune.system_prompt {
        SystemPrompt::Replace => "--system-prompt-file",
        SystemPrompt::Append => "--append-system-prompt-file",
    };
    let model = conf::persona_model(&rune.name, toml)?;
    let sys = sys_file.display().to_string();
    let mut a: Vec<String> = ["-p", "--output-format", "stream-json", "--verbose", "--include-partial-messages", "--system-prompt-snapshot", "on", flag, &sys].iter().map(|s| s.to_string()).collect();
    a.extend(["--model", &model, "--allowedTools", &rune.tools, "--dangerously-skip-permissions"].iter().map(|s| s.to_string()));
    if rune.project_instructions == "none" {
        a.extend(["--setting-sources", "user"].iter().map(|s| s.to_string()));
    }
    a.extend(["--settings".to_string(), crate::run::dwarf_settings(home, run, Some(prod).filter(|p| !p.is_empty()).map(Path::new), rune.habits, None)]);
    Ok(a)
}

/// The sweep's brief, written under `run`: the name taken, the statutes and prompt split into
/// `sweep-<rune>-<id>.system.md` and `.task.md`, and the argv the unit runs. The session reads
/// its task on stdin and writes its trace to `sweep-<rune>-<id>.log`, as `Run::sweep`'s child
/// did, so the argv is `env -i <restricted pairs> /bin/sh -c '…' sh <task> <log> <agent> <args…>`: the
/// session's environment is exactly the model's, never the manager's.
#[allow(clippy::too_many_arguments)]
pub fn brief(home: &Path, run: &Path, rune: &Rune, prompt: &str, id: &str, cfg: &BriefCfg, now: i64, base: &BTreeMap<String, String>, is_exe: &dyn Fn(&Path) -> bool, memories: &dyn Fn() -> Result<String, String>) -> Result<SweepBrief, String> {
    let release = spira_config::release_env::release_root(base.get(spira_config::RELEASE_ENV).map(String::as_str))?;
    let statutes = statutes(rune, cfg, &release, now, memories)?;
    let path = base.get("PATH").cloned().unwrap_or_default();
    let model_bin = crate::restrict::model_bin_dir(base, &path, is_exe).map_err(|e| format!("work-env: {e}"))?;
    let f = &rune.name;
    let name = crate::naming::dwarf_name_take(run, f);
    let actor = format!("dwarf-{name}");
    let prompt = if prompt.trim().is_empty() { std::fs::read_to_string(home.join("runes").join(format!("{f}.md"))).unwrap_or_default().trim_end_matches('\n').to_string() } else { prompt.to_string() };
    let sys_file = run.join(format!("sweep-{f}-{id}.system.md"));
    let task_file = run.join(format!("sweep-{f}-{id}.task.md"));
    let log = run.join(format!("sweep-{f}-{id}.log"));
    let (sys, task) = brief::split(&statutes, &prompt);
    std::fs::write(&sys_file, sys).map_err(|e| format!("{}: {e}", sys_file.display()))?;
    std::fs::write(&task_file, task).map_err(|e| format!("{}: {e}", task_file.display()))?;
    let _ = std::fs::write(run.join(format!("sweep-{f}-{id}.name")), &name);
    if cfg.agent.is_empty() {
        return Err("SPIRA_AGENT is empty — no agent to run".into());
    }
    let mut session = base.clone();
    session.insert("SPIRA_RUNE".to_string(), f.clone());
    for (k, v) in [("SPIRA_DWARF", name.as_str()), ("BEADS_ACTOR", actor.as_str()), ("GIT_AUTHOR_NAME", actor.as_str()), ("GIT_COMMITTER_NAME", actor.as_str())] {
        session.insert(k.to_string(), v.to_string());
    }
    session.insert("GIT_AUTHOR_EMAIL".to_string(), format!("{actor}@spira.local"));
    session.insert("GIT_COMMITTER_EMAIL".to_string(), format!("{actor}@spira.local"));
    let mut argv = vec![system_paths::USR_BIN_ENV.to_string(), "-i".to_string()];
    argv.extend(crate::restrict::unbound_env(&session, &model_bin).into_iter().map(|(k, v)| format!("{k}={v}")));
    argv.extend([system_paths::BIN_SH.to_string(), "-c".to_string(), "t=$1; l=$2; shift 2; exec \"$@\" <\"$t\" >>\"$l\" 2>&1".to_string(), "sh".to_string(), task_file.display().to_string(), log.display().to_string(), cfg.agent.clone()]);
    argv.extend(claude_args(home, run, &cfg.prod, rune, &sys_file, &cfg.toml)?);
    Ok(SweepBrief { name, sys_file, task_file, log, argv, env: Vec::new() })
}

/// One ledger line of a sweep: `awake <rune> <what>`, or `done <rune> sweep rc=<rc> status=<s>`
/// with the session's spend read from its log.
pub fn ledger_line(run: &Path, now: i64, rune: &str, done: Option<(i32, &str, Option<&Path>)>, awake: &str, why: Option<&str>, trace_mark: &str) {
    let l = Ledger { path: run.join("dwarf-ledger.log"), dry: false };
    match done {
        None => l.awake(now, rune, awake),
        Some((rc, status, log)) => {
            let mut rest = ledger::done_rest(rune, "sweep", rc, status, &ledger::session_result_fields(log, trace_mark));
            if let Some(w) = why {
                rest.push_str(&format!(" why={}", serde_json::to_string(w).unwrap_or_default()));
            }
            l.write(now, &rest)
        }
    }
}

/// The session's outcome from its log's last trace segment (`decide::session_outcome`).
pub fn outcome(log: &Path, trace_mark: &str) -> &'static str {
    crate::decide::session_outcome(ledger::trace_segment(Some(log), 0, trace_mark).as_deref())
}

/// Removes the sweep's brief and name files; a file already gone is success.
pub fn cleanup(run: &Path, rune: &str, id: &str) {
    for ext in ["system.md", "task.md", "name"] {
        let _ = std::fs::remove_file(run.join(format!("sweep-{rune}-{id}.{ext}")));
    }
}

/// Removes up to `max` of the sweep's run files (`sweep-<rune>-*`) older than `keep_days`,
/// sparing the current run's own.
pub fn prune(run: &Path, rune: &str, current: &str, keep_days: u32, max: usize, now: u64) -> spira_config::results_retention::Pruned {
    let all = format!("sweep-{rune}-");
    let own = format!("sweep-{rune}-{current}.");
    spira_config::results_retention::prune(run, keep_days, max, now, &|name| !name.starts_with(&all) || name.starts_with(&own))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rune_for_test() -> Rune {
        Rune::from_vars("ops", &BTreeMap::new())
    }

    fn setup(tag: &str, with_work: bool) -> (testkit::TempDir, PathBuf, PathBuf, PathBuf) {
        let root = testkit::TempDir::new(tag);
        let (home, run, rel) = (root.join("home"), root.join("run"), root.join("rel"));
        std::fs::create_dir_all(home.join("runes")).unwrap();
        std::fs::create_dir_all(&run).unwrap();
        std::fs::create_dir_all(rel.join("model-bin")).unwrap();
        if with_work {
            std::fs::write(rel.join("model-bin/work"), "").unwrap();
        }
        (root, home, run, rel)
    }

    fn base(home: &Path, rel: &Path) -> BTreeMap<String, String> {
        let h = home.display().to_string();
        [
            ("HOME", h.clone()),
            ("PATH", format!("{h}/.local/bin:{}/bin:usr/bin", rel.display())),
            ("BEADS_DIR", "db/.beads".into()),
            ("SPIRA_DB", "db".into()),
            ("SPIRA_RELEASE", rel.display().to_string()),
            ("SPIRA_LC_SOCKET", "run/lc.sock".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }

    fn cfg(home: &Path) -> BriefCfg {
        let spec = home.join("spec.toml");
        std::fs::write(&spec, "[persona.ops]\nmodel = \"claude-x\"\n").unwrap();
        BriefCfg { agent: "opt/agent".into(), toml: spec.display().to_string(), ..BriefCfg::default() }
    }

    fn exe(rel: &Path) -> impl Fn(&Path) -> bool + '_ {
        move |p| p == rel.join("model-bin/work")
    }

    #[test_id(t9hqp56)]
    #[test]
    fn the_sweep_session_runs_under_the_models_restricted_environment() {
        let (_root, home, run, rel) = setup("env", true);
        let rune = rune_for_test();
        let b = brief(&home, &run, &rune, "task", "r1", &cfg(&home), 0, &base(&home, &rel), &exe(&rel), &|| Ok("[]".into()));
        let b = b.unwrap();
        let i = b.argv.iter().position(|a| a == "-i").unwrap();
        assert_eq!(b.argv[i - 1], system_paths::USR_BIN_ENV);
        let pairs: BTreeMap<String, String> = b.argv[i + 1..].iter().take_while(|a| a.contains('=')).filter_map(|a| a.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
        assert_eq!(pairs["PATH"], format!("{}:{}/model-bin:{}/.cargo/bin", system_paths::SYSTEM_PATH_MIN, rel.display(), home.display()));
        assert_eq!(pairs["SPIRA_RUNE"], "ops");
        assert_eq!(pairs["SPIRA_LC_SOCKET"], "run/lc.sock");
        for k in ["BEADS_DIR", "SPIRA_DB", "SPIRA_WORK_BEAD_ID"] {
            assert!(!pairs.contains_key(k), "{k}");
        }
        assert!(!pairs["PATH"].split(':').any(|d| d.ends_with(".local/bin") || d.ends_with("rel/bin")));
        assert!(b.env.is_empty());
    }

    #[test_id(toc93zz)]
    #[test]
    fn a_bd_directory_and_a_credential_on_the_managers_environment_stay_out_of_the_session() {
        let (_root, home, run, rel) = setup("cred", true);
        std::fs::create_dir_all(home.join("bdstub")).unwrap();
        std::fs::write(home.join("bdstub/bd"), "").unwrap();
        let mut env = base(&home, &rel);
        env.insert("PATH".into(), format!("{}/bdstub:{}/bin", home.display(), rel.display()));
        env.insert("GH_TOKEN".into(), "secret-value".into());
        let b = brief(&home, &run, &rune_for_test(), "task", "r1", &cfg(&home), 0, &env, &exe(&rel), &|| Ok("[]".into())).unwrap();
        let joined = b.argv.join("\n");
        assert!(!joined.contains("bdstub") && !joined.contains("GH_TOKEN") && !joined.contains("secret-value"), "{joined}");
        assert!(joined.contains("SPIRA_RUNE=ops") && !joined.contains("SPIRA_WORK_BEAD_ID"));
    }

    #[test_id(teri133)]
    #[test]
    fn a_release_without_model_bin_work_refuses_the_sweep() {
        let (_root, home, run, rel) = setup("nowork", false);
        let e = brief(&home, &run, &rune_for_test(), "task", "r1", &cfg(&home), 0, &base(&home, &rel), &crate::escape::is_executable, &|| Ok("[]".into())).unwrap_err();
        assert!(e.contains("model-bin"), "{e}");
    }

    #[test_id(tif1fcq)]
    #[test]
    fn the_sweeps_statute_index_names_rule_sh_through_the_declared_release() {
        let (_root, home, run, rel) = setup("statute-release", true);
        let b = brief(&home, &run, &rune_for_test(), "task", "r1", &cfg(&home), 0, &base(&home, &rel), &exe(&rel), &|| Ok(r#"{"law-b":"B text"}"#.into())).unwrap();
        let sys = std::fs::read_to_string(b.sys_file).unwrap();
        assert!(sys.contains(&format!("{}/rule.sh show <slug-without-law-prefix>", rel.display())), "{sys}");
        assert!(!sys.contains("<harness>"), "{sys}");
    }

    #[test_id(tsiylmh)]
    #[test]
    fn a_sweep_with_no_declared_release_refuses_to_render_its_statutes() {
        let (_root, home, run, rel) = setup("statute-norelease", true);
        let mut env = base(&home, &rel);
        env.remove("SPIRA_RELEASE");
        let e = brief(&home, &run, &rune_for_test(), "task", "r1", &cfg(&home), 0, &env, &exe(&rel), &|| Ok(r#"{"law-b":"B text"}"#.into())).unwrap_err();
        assert!(e.contains("SPIRA_RELEASE is not set — it names the release"), "{e}");
    }

    const NOW: u64 = 2_000_000_000;

    fn aged(dir: &Path, name: &str, secs: u64) {
        let p = dir.join(name);
        std::fs::write(&p, "x").unwrap();
        std::fs::File::open(&p).unwrap().set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(NOW - secs)).unwrap();
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    fn planted(tag: &str) -> testkit::TempDir {
        let d = testkit::TempDir::new(tag);
        for ext in ["log", "system.md", "task.md", "name"] {
            aged(d.path(), &format!("sweep-ops-rOLD.{ext}"), 2 * 86400);
        }
        for ext in ["log", "system.md"] {
            aged(d.path(), &format!("sweep-ops-rCUR.{ext}"), 2 * 86400);
        }
        aged(d.path(), "sweep-ops-123.log", 3 * 86400);
        aged(d.path(), "sweep-ops-rNEW.log", 3600);
        aged(d.path(), "dwarf-ledger.log", 10 * 86400);
        aged(d.path(), "ops.log", 10 * 86400);
        d
    }

    #[test_id(thbcikc)]
    #[test]
    fn prune_removes_over_age_sweep_files_and_spares_the_current_run_and_other_files() {
        let d = planted("ops-prune");
        let got = prune(d.path(), "ops", "rCUR", 1, 50, NOW);
        assert_eq!((got.removed, got.left), (5, 0));
        assert_eq!(names(d.path()), ["dwarf-ledger.log", "ops.log", "sweep-ops-rCUR.log", "sweep-ops-rCUR.system.md", "sweep-ops-rNEW.log"]);
    }

    #[test_id(t7k8mhn)]
    #[test]
    fn prune_removes_at_most_max_oldest_first_and_counts_the_rest() {
        let d = planted("ops-prune-max");
        let got = prune(d.path(), "ops", "rCUR", 1, 2, NOW);
        assert_eq!((got.removed, got.left), (2, 3));
        assert!(!names(d.path()).contains(&"sweep-ops-123.log".to_string()));
    }
}
