//! `admin-store <sub>`: the one door install, deploy and operator tooling reach the beads
//! binary through. It runs in the caller's process, never over the socket, and passes bd's
//! stdout, stderr and exit code through unchanged so callers keep parsing bd's own text.

use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::CANNOT_TELL;

pub(crate) const SUBCOMMANDS: &str = "migrate | migrate-status | ping | version | dolt-start | init | config get|set <types.custom|status.custom> [value] | sql [--json] (statement on stdin)";
const CONFIG_KEYS: [&str; 2] = ["types.custom", "status.custom"];

/// What one subcommand asks of bd: its argv after `-C <db>`, whether it runs in the store's
/// directory, and whether the caller wants bd's version line followed by the binary's path.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    pub argv: Vec<String>,
    pub in_store_dir: bool,
    pub with_path: bool,
}

fn plan(argv: &[&str], in_store_dir: bool, with_path: bool) -> Plan {
    Plan { argv: argv.iter().map(|s| s.to_string()).collect(), in_store_dir, with_path }
}

pub(crate) fn plan_of(args: &[String], stdin: &mut dyn FnMut() -> Result<String, String>) -> Result<Plan, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["migrate"] => Ok(plan(&["migrate", "schema"], false, false)),
        ["migrate-status"] => Ok(plan(&["migrate", "status"], false, false)),
        ["ping"] => Ok(plan(&["sql", "select 1;"], false, false)),
        ["version"] => Ok(plan(&["version"], false, true)),
        ["dolt-start"] => Ok(plan(&["dolt", "start"], false, false)),
        ["init"] => Ok(plan(&["init", "--non-interactive", "--prefix", "sp", "--skip-agents", "--skip-hooks", "-q"], true, false)),
        ["config", "get", key] if CONFIG_KEYS.contains(key) => Ok(plan(&["config", "get", key], false, false)),
        ["config", "set", key, value] if CONFIG_KEYS.contains(key) => Ok(plan(&["config", "set", key, value], false, false)),
        ["sql"] | ["sql", "--json"] => {
            let statement = stdin()?;
            let statement = statement.trim();
            if statement.is_empty() {
                return Err("admin-store sql: the statement is read from stdin and was empty".into());
            }
            let mut p = plan(&["sql"], false, false);
            if words.len() == 2 {
                p.argv.push("--json".into());
            }
            p.argv.push(statement.to_string());
            Ok(p)
        }
        _ => Err(format!("admin-store: not a subcommand of the closed list: {SUBCOMMANDS}")),
    }
}

pub(crate) fn command(p: &Plan, bd: &str, db: &str) -> Command {
    let mut cmd = Command::new(bd);
    if p.in_store_dir {
        cmd.current_dir(db);
    } else if !db.is_empty() {
        cmd.args(["-C", db]);
    }
    cmd.args(&p.argv);
    cmd
}

fn resolved_path(bd: &str) -> String {
    if bd.contains('/') {
        return bd.to_string();
    }
    let path = std::env::var("PATH").unwrap_or_default();
    path.split(':')
        .map(|d| std::path::Path::new(d).join(bd))
        .find(|p| p.is_file())
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

/// Runs `args` (the words after `admin-store`). Returns only on a refusal or a failure to
/// start bd; otherwise bd takes this process over, so a caller's `timeout` reaches bd itself.
pub fn run(args: &[String]) -> i32 {
    let mut stdin = || {
        let mut t = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut t).map(|_| t).map_err(|e| e.to_string())
    };
    let p = match plan_of(args, &mut stdin) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return CANNOT_TELL;
        }
    };
    let (bd, db) = match (crate::bd::bd_bin(), spira_config::process::cfg("SPIRA_DB")) {
        (Ok(b), Ok(d)) => (b, d),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("admin-store: {e}");
            return CANNOT_TELL;
        }
    };
    let mut cmd = command(&p, &bd, &db);
    if p.with_path {
        return match state_store::phases::child(&bd, || cmd.output()) {
            Ok(o) => {
                let first = String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").to_string();
                println!("{first}\n{}", resolved_path(&bd));
                eprint!("{}", String::from_utf8_lossy(&o.stderr));
                o.status.code().unwrap_or(1)
            }
            Err(e) => {
                eprintln!("admin-store: running {bd}: {e}");
                CANNOT_TELL
            }
        };
    }
    let e = cmd.exec();
    eprintln!("admin-store: running {bd}: {e}");
    CANNOT_TELL
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| s.to_string()).collect()
    }

    #[test_id(t5n9pag)]
    #[test]
    fn admin_store_takes_only_its_closed_list_and_never_the_socket() {
        assert!(crate::client::try_socket(&words(&["admin-store", "migrate"])).is_none());
        assert_eq!(run(&words(&["delete"])), 2);
        assert_eq!(run(&words(&["config", "get", "issue_prefix"])), 2);
        assert_eq!(run(&words(&["config", "set", "types.custom"])), 2);
    }

    #[test_id(twxoo9a)]
    #[test]
    fn admin_store_execs_bd_with_the_stores_argv() {
        let dir = testkit::TempDir::new("admin-store-stub");
        let log = dir.join("argv");
        let stub = dir.join("bd");
        testkit::write_exe(&stub, &format!("#!/bin/sh\npwd >> '{0}'\nfor a in \"$@\"; do printf '[%s]' \"$a\" >> '{0}'; done\necho >> '{0}'\n", log.display()));
        let (bd, db) = (stub.to_str().unwrap(), dir.join("store"));
        std::fs::create_dir(&db).unwrap();
        let db = db.to_str().unwrap();
        let mut none = || Err("no stdin".to_string());
        for args in [vec!["migrate"], vec!["dolt-start"], vec!["config", "set", "status.custom", "a,b"]] {
            let p = plan_of(&words(&args), &mut none).unwrap();
            assert!(command(&p, bd, db).status().unwrap().success());
        }
        let mut sql = || Ok("select 1\n".to_string());
        let p = plan_of(&words(&["sql", "--json"]), &mut sql).unwrap();
        assert!(command(&p, bd, db).status().unwrap().success());
        let p = plan_of(&words(&["init"]), &mut none).unwrap();
        assert!(command(&p, bd, db).status().unwrap().success());
        let seen = std::fs::read_to_string(&log).unwrap();
        let want = [
            format!("[-C][{db}][migrate][schema]"),
            format!("[-C][{db}][dolt][start]"),
            format!("[-C][{db}][config][set][status.custom][a,b]"),
            format!("[-C][{db}][sql][--json][select 1]"),
            format!("{db}\n[init][--non-interactive][--prefix][sp][--skip-agents][--skip-hooks][-q]"),
        ];
        for w in &want {
            assert!(seen.contains(w), "{w} not in {seen}");
        }
        assert!(!seen.contains(&format!("[-C][{db}][init]")), "init runs in the store dir: {seen}");
    }

    #[test_id(th3i9bi)]
    #[test]
    fn the_store_callers_name_no_bd() {
        let files: [(&str, &str); 8] = [
            ("spira/deploy.sh", include_str!("../../spira/deploy.sh")),
            ("spira/bd-pin.sh", include_str!("../../spira/bd-pin.sh")),
            ("spira/schema-apply.sh", include_str!("../../spira/schema-apply.sh")),
            ("spira/schema.sh", include_str!("../../spira/schema.sh")),
            ("spira/pre-activate.sh", include_str!("../../spira/pre-activate.sh")),
            ("env_bootstrap.rs", include_str!("../../spira-config/src/env_bootstrap.rs")),
            ("stage.rs", include_str!("../../release/src/stage.rs")),
            ("tsd-lifecycle-export", include_str!("../../tsd-lifecycle-export/src/main.rs")),
        ];
        for (name, text) in files {
            let hits = bd_command_words(text);
            assert!(hits.is_empty(), "{name} still names bd: {hits:?}");
        }
    }

    fn bd_command_words(text: &str) -> Vec<&str> {
        let banned = ["bd -C", "\"${SPIRA_BD:", "command -v bd", "Command::new(\"bd-embedded\")", "bounded::bounded(&bd)"];
        text.lines().filter(|l| banned.iter().any(|b| l.contains(b)) && !l.trim_start().starts_with("//") && !l.trim_start().starts_with('#')).collect()
    }

    #[test_id(tkqoib6)]
    #[test]
    fn the_bd_matcher_finds_a_planted_offender() {
        let planted = "x=$(timeout 30 bd -C \"$DB\" migrate schema)\nCommand::new(\"bd-embedded\")\n# bd -C in a comment\n";
        assert_eq!(bd_command_words(planted).len(), 2);
    }

    #[test_id(t27rs3y)]
    #[test]
    fn version_prints_bds_first_line_then_its_path_and_a_failure_exits_2() {
        let dir = testkit::TempDir::new("admin-store-version");
        let stub = dir.join("bd");
        testkit::write_exe(&stub, "#!/bin/sh\necho 'bd version 9.9'\necho second\n");
        let mut none = || Err("no stdin".to_string());
        let p = plan_of(&["version".to_string()], &mut none).unwrap();
        let out = command(&p, stub.to_str().unwrap(), "").output().unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("bd version 9.9"));
        assert_eq!(resolved_path(stub.to_str().unwrap()), stub.to_str().unwrap());
        let mut empty = || Ok("  \n".to_string());
        assert!(plan_of(&["sql".to_string()], &mut empty).is_err());
    }
}
