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
    pub harness_repo: String,
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
pub fn statutes(rune: &Rune, cfg: &BriefCfg, now: i64, memories: &dyn Fn() -> Result<String, String>) -> Result<String, String> {
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
    let harness = if cfg.harness_repo.is_empty() { "<harness>" } else { &cfg.harness_repo };
    let missing = brief::missing_core(&json, &rune.memory_prefixes, &core);
    if !missing.is_empty() {
        return Err(format!("{}: {STATUTE_MISSING}: {} — refusing to start with a thinned brief", rune.name, missing.join(", ")));
    }
    Ok(brief::render_memories(&json, &rune.memory_prefixes, 120_000, &core, harness))
}

/// `Run::claude_argv` for a sweep: the agent's flags, without the program.
pub fn claude_args(home: &Path, rune: &Rune, sys_file: &Path, toml: &str) -> Result<Vec<String>, String> {
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
    a.extend(["--settings".to_string(), crate::run::dwarf_settings(home, rune.habits)]);
    Ok(a)
}

/// The sweep's brief, written under `run`: the name taken, the statutes and prompt split into
/// `sweep-<rune>-<id>.system.md` and `.task.md`, and the argv the unit runs. The session reads
/// its task on stdin and writes its trace to `sweep-<rune>-<id>.log`, as `Run::sweep`'s child
/// did, so the argv is `/bin/sh -c 'exec "$0" "$@" <task >>log 2>&1' <agent> <args…>`.
#[allow(clippy::too_many_arguments)]
pub fn brief(home: &Path, run: &Path, rune: &Rune, prompt: &str, id: &str, cfg: &BriefCfg, now: i64, memories: &dyn Fn() -> Result<String, String>) -> Result<SweepBrief, String> {
    let statutes = statutes(rune, cfg, now, memories)?;
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
    let mut argv = vec!["/bin/sh".to_string(), "-c".to_string(), "exec \"$0\" \"$@\" <\"$SPIRA_SWEEP_TASK\" >>\"$SPIRA_SWEEP_LOG\" 2>&1".to_string(), cfg.agent.clone()];
    argv.extend(claude_args(home, rune, &sys_file, &cfg.toml)?);
    let env = vec![
        ("SPIRA_DWARF".to_string(), name.clone()),
        ("BEADS_ACTOR".to_string(), actor.clone()),
        ("GIT_AUTHOR_NAME".to_string(), actor.clone()),
        ("GIT_AUTHOR_EMAIL".to_string(), format!("{actor}@spira.local")),
        ("GIT_COMMITTER_NAME".to_string(), actor.clone()),
        ("GIT_COMMITTER_EMAIL".to_string(), format!("{actor}@spira.local")),
        ("SPIRA_SWEEP_TASK".to_string(), task_file.display().to_string()),
        ("SPIRA_SWEEP_LOG".to_string(), log.display().to_string()),
    ];
    Ok(SweepBrief { name, sys_file, task_file, log, argv, env })
}

/// One ledger line of a sweep: `awake <rune> <what>`, or `done <rune> sweep rc=<rc> status=<s>`
/// with the session's spend read from its log.
pub fn ledger_line(run: &Path, now: i64, rune: &str, done: Option<(i32, &str, Option<&Path>)>, awake: &str, trace_mark: &str) {
    let l = Ledger { path: run.join("dwarf-ledger.log"), dry: false };
    match done {
        None => l.awake(now, rune, awake),
        Some((rc, status, log)) => l.done(now, rune, "sweep", rc, status, &ledger::session_result_fields(log, trace_mark)),
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
