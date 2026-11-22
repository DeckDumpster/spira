use crate::drive::{parse_scenario, seed_steps, ProcessExec};
use crate::world::{self, Steps};
use std::path::Path;
use std::process::{Command, Output};

pub const FIXTURE: &str = include_str!("../fixtures/pilgrim-world.toml");
const SHELL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(600); // batch-job: a tool run in a world, Loom and lc-view's loop for a whole suite

/// A command under the world's own environment and PATH — its config, socket and release — so a
/// tool run through it reads the world's store and never production's.
pub fn bound_command(world: &Path, shell: &str) -> Result<Command, String> {
    ProcessExec {
        world: world.to_path_buf(),
        epoch: parse_scenario(FIXTURE)?.epoch,
    }
    .command_with(shell, 0, false)
}

pub fn exec(world: &Path, shell: &str) -> Result<Output, String> {
    let mut cmd = bound_command(world, shell)?;
    let (status, stdout, stderr) = world::run_capture(&mut cmd, SHELL_DEADLINE)?;
    Ok(Output {
        status,
        stdout: stdout.into_bytes(),
        stderr: stderr.into_bytes(),
    })
}

/// Files the fixture's beads in the world's own store.
pub fn seed(world: &Path, fixture: &str) -> Result<(), String> {
    let scenario = parse_scenario(fixture)?;
    for (_, shell) in seed_steps(&scenario.seeds)? {
        let out = exec(world, &shell)?;
        if !out.status.success() {
            return Err(format!(
                "seeding the pilgrim world: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }
    Ok(())
}

/// A loopback address nothing listens on, for the world's own Loom.
fn free_addr() -> Result<String, String> {
    let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    l.local_addr()
        .map(|a| a.to_string())
        .map_err(|e| e.to_string())
}

/// Marks `world` a pilgrim world, seeds its store from `fixture`, and gives its Loom an address
/// of its own, so `lc-view` and `loom` run through [`exec`] read this world's socket.
pub fn up(world: &Path, fixture: &str, steps: &dyn Steps) -> Result<(), String> {
    world::make_pilgrim_world(world)?;
    let release = world.join("release");
    steps.config_set(
        &release,
        &world.join("config/sim.toml"),
        "spira.loom_addr",
        &free_addr()?,
    )?;
    crate::pilgrim_land::install_tree(&world.join("work"), world::LANDING_BASE)?;
    seed(world, fixture)
}

use crate::world::{ask_body, lc_socket, REPO_NAME};
use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};
use std::time::Instant;

const LOOM_START: std::time::Duration = std::time::Duration::from_secs(60); // batch-job: Loom binds its port and answers
const LOOM_NEEDS_LOGS_URL: &str = "http://127.0.0.1:9";

/// One `work <verb>` call as the pilgrim, over the world's own socket: `(exit code, stdout)`.
pub fn work(world: &Path, verb: &str, args: &[&str]) -> Result<(i32, String), String> {
    let mut argv = vec!["work", "-", verb];
    argv.extend_from_slice(args);
    argv.extend_from_slice(&["--actor", "pilgrim"]);
    ask_body(&lc_socket(world), &argv)
        .ok_or_else(|| format!("work {verb}: the world's lifecycle service did not answer"))
}

fn ok(world: &Path, verb: &str, args: &[&str]) -> Result<String, String> {
    match work(world, verb, args)? {
        (0, out) => Ok(out),
        (code, out) => Err(format!(
            "work {verb} {args:?} exited {code}: {}",
            out.trim()
        )),
    }
}

/// Files a builder bead as the pilgrim and returns its id.
pub fn file_bead(world: &Path, title: &str) -> Result<String, String> {
    let out = ok(
        world,
        "file",
        &[title, "--for", "builder", "--repo", REPO_NAME],
    )?;
    out.lines()
        .rev()
        .find_map(|l| l.split_whitespace().last())
        .map(str::to_string)
        .ok_or_else(|| format!("work file printed no id: {out:?}"))
}

pub fn link(world: &Path, bead: &str, depends_on: &str) -> Result<(), String> {
    ok(world, "dep-add", &[bead, depends_on]).map(|_| ())
}

pub fn close(world: &Path, bead: &str, evidence: &str) -> Result<(), String> {
    ok(world, "close-other", &[bead, "--evidence", evidence]).map(|_| ())
}

/// `lc-view once` under the world's own config and socket.
pub fn lc_view_once(world: &Path) -> Result<String, String> {
    let out = exec(world, "lc-view once")?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "lc-view once: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// A Loom serving the world's store on an ephemeral port; killed when dropped.
pub struct Loom {
    child: Child,
    addr: String,
}

impl Loom {
    pub fn start(world: &Path) -> Result<Loom, String> {
        let mut cmd = bound_command(world, "exec loom")?;
        let mut child = cmd
            .env("SPIRA_LOOM_ADDR", "127.0.0.1:0")
            .env("SPIRA_VICTORIALOGS_URL", LOOM_NEEDS_LOGS_URL)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("loom: {e}"))?;
        let stderr = child.stderr.take().ok_or("loom: no stderr")?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(addr) = line
                    .strip_prefix("loom: serving http://")
                    .and_then(|r| r.split('/').next())
                {
                    let _ = tx.send(addr.to_string());
                }
            }
        });
        let addr = rx
            .recv_timeout(LOOM_START)
            .map_err(|_| "loom did not say where it serves".to_string());
        match addr {
            Ok(addr) => Ok(Loom { child, addr }),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(e)
            }
        }
    }

    /// `GET /api/factory`, parsed.
    pub fn factory(&self) -> Result<serde_json::Value, String> {
        let url = format!("http://{}/api/factory", self.addr);
        let deadline = Instant::now() + LOOM_START;
        loop {
            let out = Command::new("curl")
                .args(["-fsS", "--max-time", "30", &url])
                .output()
                .map_err(|e| format!("curl: {e}"))?;
            if out.status.success() {
                return serde_json::from_slice(&out.stdout).map_err(|e| format!("{url}: {e}"));
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "{url}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
}

impl Drop for Loom {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `(claimable, blocked)` from the summary an lc-view pass prints; a part it leaves out is 0.
pub fn view_counts(text: &str) -> (u32, u32) {
    let after = |key: &str| {
        text.split(key)
            .nth(1)
            .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    };
    (after("claimable "), after("blocked "))
}

/// `(claimable, blocked)` from Loom's `bead.READY` belt: the floor's own count of the same beads.
pub fn loom_counts(floor: &serde_json::Value) -> Option<(u64, u64)> {
    fn find(v: &serde_json::Value) -> Option<&serde_json::Value> {
        match v {
            serde_json::Value::Object(m) => {
                if m.get("id").and_then(|i| i.as_str()) == Some("bead.READY")
                    && m.contains_key("parts")
                {
                    return m.get("parts");
                }
                m.values().find_map(find)
            }
            serde_json::Value::Array(a) => a.iter().find_map(find),
            _ => None,
        }
    }
    let parts = find(floor)?;
    Some((parts["claimable"].as_u64()?, parts["blocked"].as_u64()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(t8cfjpj)]
    #[test]
    fn the_fixture_is_a_scenario_with_a_claimable_bead_for_the_pilgrim_to_stand_beside() {
        let seeds = parse_scenario(FIXTURE).unwrap().seeds;
        assert_eq!(seeds.len(), 1);
        assert_eq!(seed_steps(&seeds).unwrap().len(), 1);
    }

    #[test_id(tvgh0bq)]
    #[test]
    fn the_counts_are_read_from_the_summary_and_a_missing_part_is_zero() {
        assert_eq!(
            view_counts("next: claimable 2 · blocked 1 ← sp-b · held 0"),
            (2, 1)
        );
        assert_eq!(view_counts("claimable 3"), (3, 0));
        assert_eq!(view_counts("nothing"), (0, 0));
    }

    #[test_id(txemdxu)]
    #[test]
    fn the_ready_belts_parts_are_found_wherever_the_floor_nests_them() {
        let floor = serde_json::json!({"factory": {"belts": [{"id": "bead.DONE", "parts": {"claimable": 9, "blocked": 9}}, {"id": "bead.READY", "parts": {"claimable": 1, "blocked": 2}}]}});
        assert_eq!(loom_counts(&floor), Some((1, 2)));
        assert_eq!(loom_counts(&serde_json::json!({})), None);
    }
}
