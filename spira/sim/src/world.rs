use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const PRODUCTION_LOCATORS: &[&str] = &["SPIRA_LC_PASSWORD_FILE", "SPIRA_LC_SOCKET", "SPIRA_RUN", "SPIRA_DB"];
const MARKER: &str = ".sim-world";
const CALL_DEADLINE: Duration = Duration::from_secs(120); // batch-job: git/tar/cp/testenv steps of building a world
const BUILD_DEADLINE: Duration = Duration::from_secs(3600); // batch-job: release build of the tree

pub fn refuse_production(env: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    let seen: Vec<&str> = PRODUCTION_LOCATORS
        .iter()
        .copied()
        .filter(|k| env(k).is_some_and(|v| !v.is_empty()))
        .collect();
    if seen.is_empty() {
        Ok(())
    } else {
        Err(format!("refusing to build a world while a production locator is set: {}", seen.join(", ")))
    }
}

/// What a world needs from outside itself: a release build and a private Dolt server.
pub trait Steps {
    fn release(&self, repo: &Path, tree: &str, cache: &Path) -> Result<PathBuf, String>;
    fn db_up(&self, world: &Path) -> Result<String, String>;
    fn db_down(&self, fixture: &str) -> Result<(), String>;
    fn config_set(&self, release: &Path, file: &Path, key: &str, value: &str) -> Result<(), String>;
}

#[derive(Default)]
pub struct ProcessSteps;

pub fn run(cmd: &mut Command, deadline: Duration) -> Result<String, String> {
    let name = format!("{:?}", cmd.get_program());
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| format!("{name}: {e}"))?;
    let mut out = child.stdout.take().ok_or("no stdout")?;
    let mut err = child.stderr.take().ok_or("no stderr")?;
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if start.elapsed() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{name}: no result within {}s", deadline.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = t_out.join().unwrap_or_default();
    let stderr = t_err.join().unwrap_or_default();
    if status.success() {
        Ok(stdout)
    } else {
        Err(format!("{name}: {status}: {}", stderr.trim()))
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    run(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "sim")
            .env("GIT_AUTHOR_EMAIL", "sim@sim.invalid")
            .env("GIT_COMMITTER_NAME", "sim")
            .env("GIT_COMMITTER_EMAIL", "sim@sim.invalid")
            .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z"),
        CALL_DEADLINE,
    )
}

impl Steps for ProcessSteps {
    fn release(&self, repo: &Path, tree: &str, cache: &Path) -> Result<PathBuf, String> {
        let hash = git(repo, &["rev-parse", &format!("{tree}^{{tree}}")])?.trim().to_string();
        let out = cache.join(&hash);
        if out.join(".complete").is_file() {
            return Ok(out);
        }
        let src = cache.join(format!("{hash}.src"));
        let _ = std::fs::remove_dir_all(&src);
        std::fs::create_dir_all(&src).map_err(|e| e.to_string())?;
        let tarball = cache.join(format!("{hash}.tar"));
        run(Command::new("git").arg("-C").arg(repo).args(["archive", "--format=tar", "-o"]).arg(&tarball).arg(tree), CALL_DEADLINE)?;
        run(Command::new("tar").arg("-xf").arg(&tarball).arg("-C").arg(&src), CALL_DEADLINE)?;
        run(
            Command::new("cargo").args(["build", "--release", "--workspace", "--bins"]).current_dir(&src),
            BUILD_DEADLINE,
        )?;
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        run(Command::new("cp").arg("-r").arg(src.join("target/release")).arg(out.join("bin")), CALL_DEADLINE)?;
        std::fs::write(out.join(".complete"), "").map_err(|e| e.to_string())?;
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_file(&tarball);
        Ok(out)
    }
    fn db_up(&self, world: &Path) -> Result<String, String> {
        let out = run(
            Command::new("testenv").args(["testdb", "up", "--tag", "simworld", "--bd", "bd", "--dolt", "dolt", "--root"]).arg(world.join("db")),
            CALL_DEADLINE,
        )?;
        out.lines()
            .find_map(|l| l.strip_prefix("TESTDB_FIXTURE="))
            .map(str::to_string)
            .ok_or_else(|| "testenv testdb up printed no TESTDB_FIXTURE".to_string())
    }

    fn config_set(&self, release: &Path, file: &Path, key: &str, value: &str) -> Result<(), String> {
        run(Command::new(release.join("bin/spira-config")).args(["set", key, value]).arg(file), CALL_DEADLINE).map(|_| ())
    }

    fn db_down(&self, fixture: &str) -> Result<(), String> {
        run(Command::new("testenv").args(["testdb", "down", "--fixture", fixture]), CALL_DEADLINE).map(|_| ())
    }
}

pub fn up(repo: &Path, dir: &Path, tree: &str, env: &dyn Fn(&str) -> Option<String>, steps: &dyn Steps) -> Result<(), String> {
    refuse_production(env)?;
    if dir.exists() && std::fs::read_dir(dir).map_err(|e| e.to_string())?.next().is_some() {
        return Err(format!("{} is not empty", dir.display()));
    }
    let repo = PathBuf::from(git(repo, &["rev-parse", "--show-toplevel"])?.trim());
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let dir = dir.canonicalize().map_err(|e| e.to_string())?;
    std::fs::write(dir.join(MARKER), "").map_err(|e| e.to_string())?;
    match build(&dir, &repo, tree, steps) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = down(&dir, steps);
            Err(e)
        }
    }
}

fn build(dir: &Path, repo: &Path, tree: &str, steps: &dyn Steps) -> Result<(), String> {
    let cache = cache_dir(dir);
    std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let release = steps.release(repo, tree, &cache)?;
    link(&release, &dir.join("release"))?;

    let fixture = steps.db_up(dir)?;
    std::fs::write(dir.join("db.fixture"), &fixture).map_err(|e| e.to_string())?;

    let origin = dir.join("origin.git");
    let work = dir.join("work");
    let p = |x: &Path| x.to_string_lossy().to_string();
    git(dir, &["init", "--bare", "--initial-branch=main", &p(&origin)])?;
    git(dir, &["init", "--initial-branch=main", &p(&work)])?;
    let tarball = dir.join("seed.tar");
    run(
        Command::new("git").arg("-C").arg(repo).args(["archive", "--format=tar", "-o"]).arg(&tarball).arg(tree),
        BUILD_DEADLINE,
    )?;
    run(Command::new("tar").arg("-xf").arg(&tarball).arg("-C").arg(&work), CALL_DEADLINE)?;
    std::fs::remove_file(&tarball).map_err(|e| e.to_string())?;
    git(&work, &["add", "-A"])?;
    git(&work, &["commit", "-q", "-m", "sim seed"])?;
    git(&work, &["remote", "add", "origin", &p(&origin)])?;
    git(&work, &["push", "-q", "origin", "main"])?;
    git(&work, &["branch", "local/main", "main"])?;
    git(&work, &["branch", "--set-upstream-to=origin/main", "main"])?;

    let run_dir = dir.join("run");
    let config = dir.join("config");
    std::fs::create_dir_all(&run_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&config).map_err(|e| e.to_string())?;
    let runner = config.join("suite-runner");
    std::fs::write(&runner, "#!/bin/sh\nexit 0\n").map_err(|e| e.to_string())?;
    set_exec(&runner)?;
    let file = config.join("sim.toml");
    for (k, v) in config_settings(&work) {
        steps.config_set(&release, &file, &k, &v)?;
    }
    std::fs::write(
        config.join("sim.env"),
        format!("SPIRA_RUN={}\nSPIRA_LIFECYCLE_ENFORCE=1\nSPIRA_SIM_GATE_RUNNER={}\n", run_dir.display(), runner.display()),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn config_settings(work: &Path) -> Vec<(String, String)> {
    let kv = |k: &str, v: &str| (k.to_string(), v.to_string());
    vec![
        kv("spira.lifecycle_enforce", "true"),
        kv("repo.sim.path", &work.display().to_string()),
        kv("repo.sim.mode", "queue.local"),
        kv("repo.sim.base", "local/main"),
    ]
}

fn cache_dir(dir: &Path) -> PathBuf {
    match std::env::var_os("SPIRA_SIM_CACHE") {
        Some(c) if !c.is_empty() => PathBuf::from(c),
        _ => dir.join("cache"),
    }
}

fn link(target: &Path, at: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, at).map_err(|e| e.to_string())
}

fn set_exec(p: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())
}

pub fn down(dir: &Path, steps: &dyn Steps) -> Result<(), String> {
    if !dir.join(MARKER).is_file() {
        return Err(format!("{} is not a sim world", dir.display()));
    }
    if let Ok(fixture) = std::fs::read_to_string(dir.join("db.fixture")) {
        steps.db_down(fixture.trim())?;
    }
    std::fs::remove_dir_all(dir).map_err(|e| e.to_string())
}
