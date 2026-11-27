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

pub const MCP_SERVERS_KEY: &str = "SPIRA_PILGRIM_MCP_SERVERS";
const SIGN_IN_KEYS: [&str; 4] = ["SPIRA_PILGRIM_ACCOUNT", "SPIRA_IDENTITY_LOGIN_HOST", "SPIRA_IDENTITY_LOGIN_CMD", "SPIRA_IDENTITY_LOGIN_URL"];

/// `repo`'s MCP server as `(name, url)`, from space-separated `repo=url` pairs.
pub fn mcp_server(map: &str, repo: &str) -> Option<(String, String)> {
    map.split_whitespace()
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, url)| *name == repo && !url.is_empty())
        .map(|(name, url)| (name.to_string(), url.to_string()))
}

/// What signing a session in to one site takes.
pub struct SignIn<'a> {
    pub name: &'a str,
    pub url: &'a str,
    pub agent: &'a str,
    pub login: &'a str,
    pub bead: &'a str,
    pub work: &'a Path,
}

fn configured(cfg: &dyn Fn(&str) -> String) -> Result<(), String> {
    let empty: Vec<String> = SIGN_IN_KEYS
        .iter()
        .filter(|k| cfg(k).trim().is_empty())
        .map(|k| k.trim_start_matches("SPIRA_").to_lowercase())
        .collect();
    if empty.is_empty() {
        return Ok(());
    }
    let exits: Vec<String> = empty.iter().map(|k| format!("`spira-config set spira.{k} <value>`")).collect();
    Err(format!("pilgrim sign-in is not configured: {} empty — set with {}", empty.join(", "), exits.join(", ")))
}

fn run_in(exec: &dyn Exec, prog: &str, args: &[&str], work: &Path, what: &str) -> Result<String, String> {
    let out = exec.exec(prog, &s(args), None, Some(work));
    if out.code != 0 {
        return Err(format!("pilgrim sign-in: {what} failed (rc {}): {}", out.code, out.stderr.trim()));
    }
    Ok(out.stdout)
}

/// Attaches the site's server and signs it in; returns what the sign-in printed (masked by it).
fn sign_in(exec: &dyn Exec, to: &SignIn) -> Result<String, String> {
    run_in(exec, to.agent, &["mcp", "add", "--transport", "http", "--scope", "local", to.name, to.url], to.work, "mcp add")?;
    // The broker mints a code only for rune pilgrim bound to a bead; `Exec` carries no environment.
    let rune = format!("SPIRA_RUNE={RUNE}");
    let bead = format!("SPIRA_WORK_BEAD_ID={}", to.bead);
    run_in(exec, "env", &[&rune, &bead, to.login, to.name, &to.work.display().to_string()], to.work, "pilgrim-login")
}

fn release(exec: &dyn Exec, from: &SignIn) {
    exec.exec(from.agent, &s(&["mcp", "remove", "--scope", "local", from.name]), None, Some(from.work));
}

/// Everything a pilgrim session needs before its launch: the world, then the site sign-in when
/// the repository has a site. Returns the model's world variables and the sign-in's output.
/// Unset sign-in config is refused before anything runs; a failure leaves nothing up.
pub fn open(exec: &dyn Exec, sim: &str, world: &Path, signin: Option<&SignIn>, cfg: &dyn Fn(&str) -> String) -> Result<(Vec<(String, String)>, String), String> {
    if signin.is_some() {
        configured(cfg)?;
    }
    let env = up(exec, sim, world)?;
    let Some(to) = signin else {
        return Ok((env, String::new()));
    };
    match sign_in(exec, to) {
        Ok(said) => Ok((env, said)),
        // The release also clears a server left behind by a session that died, which `mcp add` refuses.
        Err(e) => {
            close(exec, sim, world, signin);
            Err(e)
        }
    }
}

pub fn close(exec: &dyn Exec, sim: &str, world: &Path, signin: Option<&SignIn>) {
    if let Some(from) = signin {
        release(exec, from);
    }
    down(exec, sim, world);
}

/// Adds the server's tools to the `--allowedTools` value of `argv`.
pub fn allow_server(argv: &mut [String], name: &str) {
    if let Some(i) = argv.iter().position(|a| a == "--allowedTools") {
        if let Some(tools) = argv.get_mut(i + 1) {
            tools.push_str(&format!(",mcp__{name}"));
        }
    }
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

    #[test_id(tcamap5)]
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

    /// Records every call as `<what> @<cwd>`; `fail` names the call that exits 1.
    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<String>>,
        fail: &'static str,
    }
    impl Exec for Recorder {
        fn exec(&self, prog: &str, args: &[String], _: Option<Vec<u8>>, cwd: Option<&Path>) -> Out {
            let call = if prog == "sim" { args[..2].join(" ") } else { format!("{prog} {}", args.join(" ")) };
            let at = cwd.map(|c| format!(" @{}", c.display())).unwrap_or_default();
            self.calls.lock().unwrap().push(format!("{call}{at}"));
            if call == "world up" {
                let dir = Path::new(&args[2]).join("config");
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("sim.env"), ENV).unwrap();
            }
            if !self.fail.is_empty() && call.contains(self.fail) {
                return Out { code: 1, stdout: String::new(), stderr: "the sign-in broke".into() };
            }
            Out::ok("")
        }
    }
    const MAP: &str = "other=https://o.test/mcp dumpstar=https://x.test/mcp";

    fn signed_in<'a>(site: &'a (String, String), work: &'a Path) -> SignIn<'a> {
        SignIn { name: &site.0, url: &site.1, agent: "claude", login: "pilgrim-login", bead: "sp-walk", work }
    }

    fn all_set(k: &str) -> String {
        format!("set-{k}")
    }

    #[test_id(t1x6glb)]
    #[test]
    fn a_repo_with_a_site_is_signed_in_after_its_world_is_up_and_released_after() {
        let t = testkit::TempDir::new("pilgrim-signin");
        let exec = Recorder::default();
        let site = mcp_server(MAP, "dumpstar");
        let work = Path::new("wt");
        let signin = site.as_ref().map(|s| signed_in(s, work));
        open(&exec, "sim", &t.join("w"), signin.as_ref(), &all_set).unwrap();
        assert_eq!(
            *exec.calls.lock().unwrap(),
            [
                "world up",
                "claude mcp add --transport http --scope local dumpstar https://x.test/mcp @wt",
                "env SPIRA_RUNE=pilgrim SPIRA_WORK_BEAD_ID=sp-walk pilgrim-login dumpstar wt @wt",
            ]
        );
        exec.calls.lock().unwrap().clear();
        close(&exec, "sim", &t.join("w"), signin.as_ref());
        assert_eq!(*exec.calls.lock().unwrap(), ["claude mcp remove --scope local dumpstar @wt", "world down"]);
    }

    #[test_id(t5sba6a)]
    #[test]
    fn empty_sign_in_config_is_refused_naming_every_empty_key_before_anything_runs() {
        let t = testkit::TempDir::new("pilgrim-signin-unset");
        let exec = Recorder::default();
        let site = mcp_server(MAP, "dumpstar").unwrap_or_default();
        let signin = signed_in(&site, Path::new("wt"));
        let cfg = |k: &str| if k == "SPIRA_IDENTITY_LOGIN_URL" || k == "SPIRA_PILGRIM_ACCOUNT" { String::new() } else { all_set(k) };
        let e = open(&exec, "sim", &t.join("w"), Some(&signin), &cfg).map(|_| ()).expect_err(&format!("not refused; calls {:?}", exec.calls.lock().unwrap()));
        assert!(e.contains("spira-config set spira.identity_login_url <value>") && e.contains("spira-config set spira.pilgrim_account <value>"), "{e}");
        assert!(!e.contains("identity_login_host") && !e.contains("identity_login_cmd"), "{e}");
        assert!(exec.calls.lock().unwrap().is_empty(), "{:?}", exec.calls.lock().unwrap());
    }

    #[test_id(tccth7e)]
    #[test]
    fn a_failed_sign_in_carries_its_stderr_and_takes_the_world_down() {
        let t = testkit::TempDir::new("pilgrim-signin-fail");
        let exec = Recorder { fail: "pilgrim-login", ..Default::default() };
        let site = mcp_server(MAP, "dumpstar").unwrap_or_default();
        let signin = signed_in(&site, Path::new("wt"));
        let e = open(&exec, "sim", &t.join("w"), Some(&signin), &all_set).map(|_| ()).expect_err(&format!("no error; calls {:?}", exec.calls.lock().unwrap()));
        assert!(e.contains("the sign-in broke"), "{e}");
        let calls = exec.calls.lock().unwrap();
        assert!(calls.iter().any(|c| c.starts_with("claude mcp remove")), "{calls:?}");
        assert_eq!(calls.last().map(String::as_str), Some("world down"));
    }

    #[test_id(t00hw3l)]
    #[test]
    fn a_repo_with_no_site_gets_only_its_world_even_with_no_sign_in_config() {
        let t = testkit::TempDir::new("pilgrim-signin-nosite");
        let exec = Recorder::default();
        assert_eq!(mcp_server(MAP, "spira"), None);
        open(&exec, "sim", &t.join("w"), None, &|_| String::new()).unwrap();
        assert_eq!(*exec.calls.lock().unwrap(), ["world up"]);
    }
}
