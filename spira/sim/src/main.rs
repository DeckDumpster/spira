use spira_sim::gh;
use spira_sim::world::{self, ProcessSteps};
use std::path::PathBuf;

const USAGE: &str = "usage: sim world up <dir> [--tree <rev>]\n       sim world down <dir>\n       sim gh <gh arguments...>\n       sim round-vm run <tree> --results-dir <dir>\n       sim ghctl <state-dir> <verb> ...\n       sim run <scenario|name> [--seed N] [--world <dir>] [--keep]\n       sim step <dir> [--until <vtime|bead:<bead>:<STATE>>]\n       sim replay <dir> --seed N\n       sim probe <dir>
       sim summon
       sim ci run <workflow.yml> <job> --sha <sha> --workspace <dir> [--skip <step name>]... [--path <dir>]... [--env KEY=VALUE]...";

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    let invoked_as = args.first().and_then(|a| std::path::Path::new(a).file_name()).and_then(|n| n.to_str()).map(str::to_string);
    args.remove(0);
    if invoked_as.as_deref().is_some_and(|n| world::INERT_TOOLS.contains(&n)) {
        std::process::exit(0);
    }
    if let Some(name @ ("gh" | "round-vm" | "cargo")) = invoked_as.as_deref() {
        args.insert(0, name.to_string());
    }
    let code = match run(&args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("sim: {e}");
            if e.starts_with("usage") { 2 } else { 1 }
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let env = |k: &str| std::env::var(k).ok();
    let state_dir = || std::env::var_os(gh::STATE_ENV).filter(|v| !v.is_empty()).map(PathBuf::from).ok_or(format!("{} is not set: sim gh answers only inside a world", gh::STATE_ENV));
    match args.first().map(String::as_str) {
        Some("gh") => return gh_main(&state_dir()?, &cwd, &args[1..]),
        Some("round-vm") => return round_vm_main(&args[1..], &env),
        Some("cargo") => return spira_sim::ci::cargo_shim(&args[1..], &cwd, &env),
        Some("ci") => return ci_main(&args[1..], &env),
        Some("ghctl") => {
            let (dir, rest) = args[1..].split_first().ok_or(USAGE.to_string())?;
            print!("{}", gh::ctl(&PathBuf::from(dir), rest)?);
            return Ok(());
        }
        Some("run") => return run_main(&cwd, &args[1..], &env),
        Some("step") => return step_main(&args[1..]),
        Some("replay") => return replay_main(&args[1..]),
        Some("summon") => return summon_main(&env),
        Some("probe") => {
            let [dir] = &args[1..] else { return Err(USAGE.to_string()) };
            print!("{}", spira_sim::probe::probe(&PathBuf::from(dir), &env)?);
            return Ok(());
        }
        _ => {}
    }
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["world", "up", dir] => world::up(&cwd, &PathBuf::from(dir), "HEAD", &env, &ProcessSteps::default()),
        ["world", "up", dir, "--tree", rev] => world::up(&cwd, &PathBuf::from(dir), rev, &env, &ProcessSteps::default()),
        ["world", "down", dir] => world::down(&PathBuf::from(dir), &ProcessSteps::default()),
        _ => Err(USAGE.to_string()),
    }
}

fn gh_main(state: &std::path::Path, cwd: &std::path::Path, args: &[String]) -> Result<(), String> {
    use std::io::Write;
    let out = gh::run(state, cwd, args)?;
    let _ = std::io::stdout().write_all(&out.stdout);
    eprint!("{}", out.stderr);
    if out.code != 0 {
        std::process::exit(out.code);
    }
    Ok(())
}

fn round_vm_main(args: &[String], env: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    use spira_sim::roundvm;
    let world = env(roundvm::WORLD_ENV).filter(|v| !v.is_empty()).ok_or(format!("{} is not set: sim round-vm answers only inside a world", roundvm::WORLD_ENV))?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs();
    let (code, err) = roundvm::run(&PathBuf::from(world), args, env, now);
    eprint!("{err}");
    std::process::exit(code);
}

fn ci_main(args: &[String], env: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    use spira_sim::ci::{run_job, steps, Context, Job};
    let usage = || USAGE.to_string();
    let (verb, rest) = args.split_first().ok_or_else(usage)?;
    if verb != "run" {
        return Err(usage());
    }
    let (mut pos, mut skip, mut path, mut sha, mut workspace) = (Vec::new(), Vec::new(), Vec::new(), None, None);
    let mut image_env: Vec<(String, String)> = Vec::new();
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--skip" => skip.push(it.next().ok_or("usage: --skip needs a step name")?.clone()),
            "--env" => {
                let kv = it.next().ok_or("usage: --env needs KEY=VALUE")?;
                image_env.push(kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).ok_or("usage: --env needs KEY=VALUE")?);
            }
            "--path" => path.push(PathBuf::from(it.next().ok_or("usage: --path needs a directory")?)),
            "--sha" => sha = Some(it.next().ok_or("usage: --sha needs a value")?.clone()),
            "--workspace" => workspace = Some(PathBuf::from(it.next().ok_or("usage: --workspace needs a value")?)),
            f if f.starts_with("--") => return Err(format!("usage: unknown flag {f}")),
            p => pos.push(p.to_string()),
        }
    }
    let ([workflow, job], Some(sha), Some(workspace)) = (pos.as_slice(), sha, workspace) else { return Err(usage()) };
    let need = |k: &str| env(k).filter(|v| !v.is_empty()).ok_or(format!("{k} is not set: sim ci answers only inside a world"));
    let gh_dir = PathBuf::from(need(gh::STATE_ENV)?);
    let release_bin = PathBuf::from(need("SPIRA_RELEASE")?).join("bin");
    let text = std::fs::read_to_string(workflow).map_err(|e| format!("{workflow}: {e}"))?;
    let steps = steps(&text, job)?;
    for s in &skip {
        if !steps.iter().any(|st| &st.name == s) {
            return Err(format!("job {job} has no step {s:?} to skip"));
        }
    }
    let scratch = std::env::temp_dir().join(format!("sim-ci-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
    let sim = std::env::current_exe().map_err(|e| e.to_string())?;
    let workspace = workspace.canonicalize().map_err(|e| format!("{}: {e}", workspace.display()))?;
    let j = Job { steps: &steps, skip: &skip, cx: Context { sha: &sha, workspace: &workspace }, scratch: &scratch, gh_dir: &gh_dir, release_bin: &release_bin, path: &path, env: &image_env };
    let code = run_job(&j, &sim, |l| print!("{l}"))?;
    let _ = std::fs::remove_dir_all(&scratch);
    if code == 0 { Ok(()) } else { Err(format!("job {job} failed with exit {code}")) }
}

fn summon_main(env: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    use spira_sim::agent::{SCENARIO_VAR, STATE_VAR};
    let need = |k: &str| env(k).filter(|v| !v.is_empty()).ok_or(format!("{k} is not set: sim summon answers only inside a world"));
    let world = need(spira_sim::roundvm::WORLD_ENV)?;
    let ran = spira_sim::summon::summon(&PathBuf::from(world), &PathBuf::from(need(SCENARIO_VAR)?), &PathBuf::from(need(STATE_VAR)?))?;
    for id in ran {
        println!("sim summon: ran the stub agent for {id}");
    }
    Ok(())
}

fn flags(args: &[String], known: &[&str], bare: &[&str]) -> Result<(Vec<String>, std::collections::BTreeMap<String, String>), String> {
    let (mut pos, mut set) = (Vec::new(), std::collections::BTreeMap::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if bare.contains(&a.as_str()) {
            set.insert(a.clone(), String::new());
        } else if known.contains(&a.as_str()) {
            set.insert(a.clone(), it.next().ok_or(format!("usage: {a} needs a value"))?.clone());
        } else if a.starts_with("--") {
            return Err(format!("usage: unknown flag {a}"));
        } else {
            pos.push(a.clone());
        }
    }
    Ok((pos, set))
}

fn seed_of(set: &std::collections::BTreeMap<String, String>) -> Result<Option<u64>, String> {
    set.get("--seed").map(|s| s.parse().map_err(|_| format!("usage: --seed {s:?} is not a number"))).transpose()
}

fn report(r: &spira_sim::verbs::RunResult) -> Result<(), String> {
    let failures = r.failures();
    if failures.is_empty() {
        println!("sim: ok seed {}", r.seed);
        return Ok(());
    }
    Err(failures.join("\nsim: "))
}

fn run_main(cwd: &std::path::Path, args: &[String], env: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    use spira_sim::drive::{ProcessExec, ProcessProbe, parse_scenario};
    let (pos, set) = flags(args, &["--seed", "--world"], &["--keep"])?;
    let [scenario] = pos.as_slice() else { return Err(USAGE.to_string()) };
    let path = spira_sim::drive::scenario_path(cwd, scenario);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let sc = parse_scenario(&text)?;
    let seed = match seed_of(&set)? {
        Some(s) => s,
        None => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos() as u64,
    };
    world::refuse_production(env)?;
    let (dir, owned) = match set.get("--world") {
        Some(w) => (PathBuf::from(w), false),
        None => {
            let d = std::env::temp_dir().join(format!("sim-run-{}-{seed}", std::process::id()));
            world::up(cwd, &d, "HEAD", env, &ProcessSteps::default())?;
            (d, true)
        }
    };
    let probe = ProcessProbe::for_world(&dir, sc.epoch)?;
    if sc.goal.is_some() && probe.probe.is_none() {
        return Err(format!("the world configures no SIM_PROBE, so goal {:?} cannot be observed", sc.goal.unwrap_or_default()));
    }
    let exec = ProcessExec { world: dir.clone(), epoch: sc.epoch };
    let result = spira_sim::verbs::run_scenario(&dir, &text, seed, Box::new(exec), Box::new(probe));
    let outcome = result.and_then(|r| report(&r));
    if owned && outcome.is_ok() && !set.contains_key("--keep") {
        world::down(&dir, &ProcessSteps::default())?;
    } else {
        eprintln!("sim: world kept at {}", dir.display());
    }
    outcome
}

fn step_main(args: &[String]) -> Result<(), String> {
    use spira_sim::drive::{ProcessExec, ProcessProbe};
    let (pos, set) = flags(args, &["--until"], &[])?;
    let [dir] = pos.as_slice() else { return Err(USAGE.to_string()) };
    let dir = PathBuf::from(dir);
    let until = set.get("--until").map(|u| spira_sim::verbs::parse_until(u)).transpose()?;
    let text = std::fs::read_to_string(dir.join("scenario.toml")).map_err(|e| format!("{}: {e}", dir.display()))?;
    let epoch = spira_sim::drive::parse_scenario(&text)?.epoch;
    let probe = ProcessProbe::for_world(&dir, epoch)?;
    let exec = ProcessExec { world: dir.clone(), epoch };
    report(&spira_sim::verbs::step_world(&dir, until, Box::new(exec), Box::new(probe))?)
}

fn replay_main(args: &[String]) -> Result<(), String> {
    let (pos, set) = flags(args, &["--seed"], &[])?;
    let ([dir], Some(seed)) = (pos.as_slice(), seed_of(&set)?) else { return Err(USAGE.to_string()) };
    match spira_sim::verbs::replay_world(&PathBuf::from(dir), seed)? {
        None => {
            println!("sim: replay seed {seed}: no divergence");
            Ok(())
        }
        Some(d) => Err(format!(
            "replay seed {seed} diverges at seq {}: recorded {} replayed {}",
            d.seq,
            d.recorded.as_deref().unwrap_or("(none)"),
            d.replayed.as_deref().unwrap_or("(none)")
        )),
    }
}
