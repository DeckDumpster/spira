//! A pilgrim's world: a store of its own, a scratch registry and a fixture repository, built
//! by `sim world up --pilgrim` before the pilgrim's session and taken down after it. The
//! session keeps the production socket, which its own bead lives in; the world is named
//! beside it so a walk that registers a repo or lands a bead never writes production.

use std::path::{Path, PathBuf};

use crate::ports::{s, Exec};

pub const RUNE: &str = "pilgrim";
pub const WORLD_ENV: &str = "PILGRIM_WORLD";
pub const SOCKET_ENV: &str = "PILGRIM_LC_SOCKET";
pub const GRAFANA_URL_ENV: &str = "PILGRIM_GRAFANA_URL";
pub const TOML_ENV: &str = "PILGRIM_TOML";
const FIXTURE_REPO: &str = "fixture:push";
const SIM_ENV: &str = "config/sim.env";

pub fn world_dir(run_dir: &Path, bead: &str) -> PathBuf {
    run_dir.join(format!("{bead}.pilgrim-world"))
}

/// The variables that name `world` to the model, read from the `sim.env` its build wrote.
pub fn model_env(world: &Path, sim_env: &str) -> Result<Vec<(String, String)>, String> {
    let get = |key: &str| {
        sim_env
            .lines()
            .filter_map(|l| l.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{} names no {key}", world.join(SIM_ENV).display()))
    };
    let mut env = vec![
        (WORLD_ENV.to_string(), world.display().to_string()),
        (SOCKET_ENV.to_string(), get("SPIRA_LC_SOCKET")?),
        (TOML_ENV.to_string(), get(spira_config::process::TOML_ENV)?),
    ];
    env.extend(get(GRAFANA_URL_ENV).ok().map(|u| (GRAFANA_URL_ENV.to_string(), u)));
    Ok(env)
}

/// Builds the world and returns what the model is told about it. A world that does not come up
/// is an error, never a session that walks production.
pub fn up(exec: &dyn Exec, sim: &str, world: &Path) -> Result<Vec<(String, String)>, String> {
    let out = exec.exec(sim, &s(&["world", "up", &world.display().to_string(), "--pilgrim", "--repo", FIXTURE_REPO]), None, None);
    if out.code != 0 {
        down(exec, sim, world);
        return Err(format!("pilgrim world did not come up (rc {}): {}", out.code, out.stderr.trim()));
    }
    let text = std::fs::read_to_string(world.join(SIM_ENV)).map_err(|e| format!("{}: {e}", world.join(SIM_ENV).display()))?;
    model_env(world, &text).inspect_err(|_| down(exec, sim, world))
}

pub fn down(exec: &dyn Exec, sim: &str, world: &Path) {
    exec.exec(sim, &s(&["world", "down", &world.display().to_string()]), None, None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Out;
    use std::sync::Mutex;

    struct FakeSim {
        calls: Mutex<Vec<String>>,
        up_rc: i32,
        sim_env: &'static str,
    }
    impl Exec for FakeSim {
        fn exec(&self, _: &str, args: &[String], _: Option<Vec<u8>>, _: Option<&Path>) -> Out {
            self.calls.lock().unwrap().push(args[..2].join(" "));
            if args[..2] == ["world", "up"] && self.up_rc == 0 {
                let dir = Path::new(&args[2]).join("config");
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("sim.env"), self.sim_env).unwrap();
            }
            Out { code: if args[1] == "up" { self.up_rc } else { 0 }, stdout: String::new(), stderr: "boom".into() }
        }
    }
    const ENV: &str = "SPIRA_LC_SOCKET=w/lc.sock\nSPIRA_TOML=w/config/sim.toml\nOTHER=1\n";

    #[test_id(t5gmkjd)]
    #[test]
    fn a_pilgrim_session_is_told_the_worlds_socket_and_registry_not_production() {
        let t = testkit::TempDir::new("pilgrim-world-up");
        let sim = FakeSim { calls: Default::default(), up_rc: 0, sim_env: ENV };
        let env = up(&sim, "sim", &t.join("w")).unwrap();
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get(SOCKET_ENV), Some("w/lc.sock"));
        assert_eq!(get(TOML_ENV), Some("w/config/sim.toml"));
        assert_eq!(get(WORLD_ENV), Some(t.join("w").display().to_string().as_str()));
        assert!(env.iter().all(|(k, _)| k != "SPIRA_LC_SOCKET"), "production's socket is the session's, untouched");
    }

    #[test_id(t3axk4y)]
    #[test]
    fn a_session_is_told_the_grafana_url_only_when_the_world_has_a_sign_in() {
        let w = Path::new("w");
        let has = |t: &str| model_env(w, t).unwrap().iter().any(|(k, v)| k == GRAFANA_URL_ENV && v == "http://g:1");
        assert!(has(&format!("{ENV}PILGRIM_GRAFANA_URL=http://g:1\n")));
        assert!(!has(ENV));
    }

    #[test]
    fn a_world_that_does_not_come_up_refuses_and_is_taken_down() {
        let t = testkit::TempDir::new("pilgrim-world-fail");
        let sim = FakeSim { calls: Default::default(), up_rc: 1, sim_env: ENV };
        let e = up(&sim, "sim", &t.join("w")).unwrap_err();
        assert!(e.contains("did not come up") && e.contains("boom"), "{e}");
        assert_eq!(sim.calls.lock().unwrap().last().map(String::as_str), Some("world down"));
    }

    #[test_id(ts4b7qw)]
    #[test]
    fn a_world_whose_env_names_no_socket_is_refused() {
        let t = testkit::TempDir::new("pilgrim-world-nosock");
        let sim = FakeSim { calls: Default::default(), up_rc: 0, sim_env: "SPIRA_TOML=w/t\n" };
        assert!(up(&sim, "sim", &t.join("w")).unwrap_err().contains("SPIRA_LC_SOCKET"));
    }
}
