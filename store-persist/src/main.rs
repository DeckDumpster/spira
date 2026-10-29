//! store-persist restore | pass [--offline] | run | seed --from DIR | compact | age
//!
//! Exit 0 done; 1 the operation failed (or `age` is past the bound); 2 usage or config.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use store_persist::ports::{Clock, Conn, Dolt};
use store_persist::{Config, Mode};

const USAGE: &str = "usage: store-persist restore | pass [--offline] | run | seed --from DIR | compact | age";

struct RealClock;

impl Clock for RealClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
    }
    fn sleep(&self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
}

struct RealDolt;

impl RealDolt {
    /// SQL goes on stdin; the server's local-user secret in the environment, never argv.
    fn sql(&self, mut cmd: Command, sql: &str) -> Result<String, String> {
        cmd.envs(spira_config::release_env::child_path_env_for_process());
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("cannot run dolt: {e}"))?;
        child
            .stdin
            .take()
            .ok_or("dolt has no stdin")?
            .write_all(sql.as_bytes())
            .map_err(|e| format!("cannot write to dolt: {e}"))?;
        let out = child.wait_with_output().map_err(|e| format!("dolt did not finish: {e}"))?;
        if !out.status.success() {
            return Err(format!("dolt exited {}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

impl Dolt for RealDolt {
    fn sync(&self, conn: &Conn, data: &Path, db: &str, dest: &Path) -> Result<(), String> {
        // batch-job: a sync copies whatever the copy lacks, a whole store when it starts empty
        let mut cmd = Command::new("dolt");
        match conn {
            Conn::Server { port, secret } => {
                cmd.env("DOLT_CLI_PASSWORD", secret)
                    .args(["--host", "127.0.0.1", "--port", &port.to_string(), "--no-tls", "-u", "__dolt_local_user__", "--use-db", db, "sql"]);
            }
            Conn::Offline => {
                cmd.current_dir(data.join(db)).arg("sql");
            }
        }
        let url = format!("file://{}", dest.display());
        if url.contains('\'') {
            return Err(format!("{} cannot be quoted as a backup URL", dest.display()));
        }
        self.sql(cmd, &format!("CALL DOLT_BACKUP('sync-url', '{url}');\n")).map(|_| ())
    }

    fn verify(&self, db_dir: &Path) -> Result<String, String> {
        // batch-job: opening a store loads its table-file indexes, proportional to its size
        let mut cmd = Command::new("dolt");
        cmd.current_dir(db_dir).args(["sql", "-r", "csv"]);
        let out = self.sql(cmd, "SELECT dolt_hashof_db() AS h;\n")?;
        out.lines().nth(1).map(str::to_string).filter(|h| !h.is_empty()).ok_or_else(|| format!("no hash from {}", db_dir.display()))
    }

    fn alive(&self, pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/comm")).map(|c| c.trim() == "dolt").unwrap_or(false)
    }
}

fn config() -> Result<Config, String> {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let home = spira_config::resolve::locate_home(&env, &std::env::current_exe().unwrap_or_default())?;
    let repo = spira_config::resolve::derive_home_repo(&home, &env);
    let r = spira_config::resolve::resolve_for_process(&home, &repo, &env)?;
    Config::from_values(|k| r.values.get(k).cloned())
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let sink = spira_otel::metrics::MetricsSink::for_component(spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok().as_deref(), "spira-store-persist");
    let operation = argv.first().map_or("none", String::as_str).to_string();
    ExitCode::from(spira_otel::metrics::instrument(&sink, "store-persist", &operation, || i32::from(run(&argv, &sink))) as u8)
}

fn run(argv: &[String], sink: &spira_otel::metrics::MetricsSink) -> u8 {
    if argv.first().map(String::as_str) == Some("check-config") {
        return spira_config::process::check_config_verb("store-persist", &[]) as u8;
    }
    if argv.first().map(String::as_str) == Some("run") {
        spira_config::process::check_config_or_exit("store-persist", &[]);
    }
    let cfg = match config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("store-persist: {e}");
            return 2;
        }
    };
    let (dolt, clock) = (RealDolt, RealClock);
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    let result: Result<Vec<String>, String> = match args.as_slice() {
        ["restore"] => store_persist::restore(&cfg, &dolt).map(|r| {
            let mut out = vec![format!("store-persist: restored [{}], kept [{}]", r.restored.join(","), r.kept.join(","))];
            out.extend(r.notes);
            out
        }),
        ["pass"] | ["pass", "--offline"] if cfg.durable.is_none() => store_persist::unconfigured(&cfg).map(|n| vec![format!("store-persist: {n}")]),
        ["pass"] | ["pass", "--offline"] => {
            let mode = if args.len() == 2 { Mode::Offline } else { Mode::Online };
            store_persist::pass(&cfg, mode, &dolt, &clock).map(|st| vec![format!("store-persist: pass {}..{} synced {}", st.start_ms, st.finish_ms, st.dbs.join(","))])
        }
        ["run"] => {
            store_persist::run(&cfg, &dolt, &clock, None, &mut |l| eprintln!("{l}"));
            Ok(vec![])
        }
        ["seed", "--from", dir] => store_persist::seed(&cfg, &PathBuf::from(dir), &dolt, &clock)
            .map(|st| vec![format!("store-persist: seeded {} from {dir}", st.dbs.join(","))]),
        ["compact"] => store_persist::compact(&cfg, &dolt).map(|v| v.into_iter().map(|l| format!("store-persist: {l}")).collect()),
        ["age"] => store_persist::age(&cfg, clock.now_ms()).and_then(|(lag, stale)| {
            sink.push(&[spira_otel::metrics::Point::Gauge { name: "spira_store_persist_lag_seconds", value: lag as f64 / 1000.0 }], &[("component", "store-persist")]);
            if stale {
                Err(format!("newest pass began {lag} ms ago, past the {}s bound — writes since then are not durable", cfg.max_lag_secs))
            } else {
                Ok(vec![format!("store-persist: newest pass began {lag} ms ago (bound {}s)", cfg.max_lag_secs)])
            }
        }),
        _ => {
            eprintln!("store-persist: {USAGE}");
            return 2;
        }
    };
    match result {
        Ok(lines) => {
            for l in lines {
                println!("{l}");
            }
            0
        }
        Err(e) => {
            eprintln!("store-persist: {e}");
            1
        }
    }
}
