//! `bead` — see DESIGN.md. `spira/bead.sh` is the shim every caller names; it `exec`s into
//! this binary by bare name on the release PATH, passing its own directory as `--home`
//! (the same convention `gate.sh` established).

use std::collections::HashMap;
use std::env;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use bead::{
    branch_candidate, branch_label, chamber_partitions, incident_blocks_refusal, is_blocks_type,
    lane_check, lint_judge, non_work_labels, parse_blocks_targets, parse_list_ids, parse_show_row,
    persona_line, repos_by_name, repos_section, work_labels, LaneCheck,
};

fn main() {
    let argv: Vec<String> = env::args().skip(1).collect();
    let mut home = ".".to_string();
    let mut rest: Vec<String> = Vec::new();
    let mut it = argv.into_iter();
    while let Some(a) = it.next() {
        if a == "--home" {
            home = it.next().unwrap_or_else(|| ".".to_string());
        } else {
            rest.push(a);
        }
    }

    let mut args = rest.into_iter();
    let cmd = args.next().unwrap_or_default();
    let rest: Vec<String> = args.collect();

    let rc = match cmd.as_str() {
        "file" => cmd_file(&home, &rest),
        "amend" => cmd_amend(&home, &rest),
        "contract" => cmd_contract(&home),
        "lint" => cmd_lint(&home, &rest),
        "dep" => match rest.first().map(String::as_str) {
            Some("add") => cmd_dep_add(&home, &rest[1..]),
            _ => {
                eprintln!("usage: bead.sh dep add <id> <depends-on-id> [--type <type>]");
                2
            }
        },
        _ => {
            eprintln!(
                "usage: bead.sh file \"<title>\" --for <persona> --repo <name> [--priority N] [--body-file F] [--express] [--json]\n       bead.sh file \"<title>\" --kind <kind> [--repo <name>] [--priority N] [--body-file F] [--express] [--json]\n       bead.sh amend <id> [--note \"<text>\"] [--body-file F] [--express]\n       bead.sh dep add <id> <depends-on-id> [--type <type>]\n       bead.sh lint [--all|<id>...]\n       bead.sh contract"
            );
            2
        }
    };
    std::process::exit(rc);
}

// =========================================================================================
// Subprocess bridges — everything that is lib.sh's/schema.sh's/mail's own logic, not
// bead.sh's. See DESIGN.md "What ported vs what stayed bash".
// =========================================================================================

/// `. "$home/lib.sh"; bdq "$@"` — the ONE subprocess boundary this crate crosses into bash
/// logic (the create-time safety fences, the connection retry loop). `$1` carries `home`
/// so the script needs no string-interpolation of an untrusted path.
///
/// NOT COLLAPSED onto a direct `bdq`-binary call (sp-pwmlj, wave 4.15 — considered and
/// rejected). `lib.sh`'s own `bdq()` is already a one-line shim onto `bead::bdq`
/// (sp-w3h16), so this bridge has no competing Rust logic to retire — every `bdq_status`/
/// `bdq_capture` call already reaches the one real implementation, just by way of a bash
/// hop. That hop is still load-bearing for `cmd_file`'s `bd create`: `bead.sh` (the shim
/// that execs into this binary) re-exports only `SPIRA_HOME`/`SPIRA_REPO_MAP` of the five
/// vars the repo-label fence needs — `SPIRA_HOME_REPO`/`SPIRA_REPO_DERIVED` are not, because
/// `bead.sh` only patches up the two every fixture's repository map already depends on. This
/// script sources `lib.sh` (hence `conf.sh`) fresh inside the bash subprocess, which
/// re-derives all five correctly before its own `bdq()` shim threads them across the exec
/// boundary to the binary — the same exec-boundary trap this wave's rules name, worked
/// around here by re-resolving in bash rather than in this process. Collapsing it for real
/// needs `spira-config resolve` in-process first (wave4-decomposition.md rows 4–6, not yet
/// landed) so this binary can build the registry itself, the way `aeon`/`cockpit-collect`
/// already do.
const BDQ_SCRIPT: &str = r#"home="$1"; shift; . "$home/lib.sh" || exit 90; bdq "$@""#;

/// Runs `bdq` with inherited stdio (the shape every state-changing call needs: `bd`'s own
/// stdout/stderr must reach the original caller exactly as it would running the bash).
fn bdq_status(home: &str, args: &[String]) -> i32 {
    Command::new("bash")
        .arg("-c")
        .arg(BDQ_SCRIPT)
        .arg("bdq")
        .arg(home)
        .args(args)
        .status()
        .map(|s| s.code().unwrap_or(1))
        .unwrap_or(127)
}

/// Runs `bdq` capturing stdout, discarding stderr (`2>/dev/null`, matching every read call
/// `bead.sh`'s own sweep made).
fn bdq_capture(home: &str, args: &[String]) -> (i32, String) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(BDQ_SCRIPT)
        .arg("bdq")
        .arg(home)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    match out {
        Ok(o) => (
            o.status.code().unwrap_or(1),
            String::from_utf8_lossy(&o.stdout).into_owned(),
        ),
        Err(_) => (127, String::new()),
    }
}

fn s(strs: &[&str]) -> Vec<String> {
    strs.iter().map(|s| s.to_string()).collect()
}

/// `spira_config::resolve::resolve_for_process`, called in-process (wave 4.9, sp-k80sa:
/// this replaces `bead.sh`'s own narrow `export SPIRA_HOME SPIRA_REPO_MAP
/// SPIRA_GROOMER_LABEL SPIRA_MAECHEN_LABEL SPIRA_CZAR_LABEL`, which only existed to carry
/// those across the `exec` boundary into this binary). `home` is this process's own
/// `--home` argument, never read back out of `$SPIRA_HOME` — that var is a per-copy fact
/// `spira_config::resolve` deliberately never derives, so it must be the caller's own
/// input, not something this resolves. A failure (no config document resolves, or a parse
/// error) yields an empty `Resolved`, matching this crate's existing "missing config
/// degrades to the caller's own default" behaviour everywhere else.
fn resolved_config(home: &str) -> spira_config::resolve::Resolved {
    let home_path = Path::new(home);
    let env_map: std::collections::BTreeMap<String, String> = env::vars().collect();
    let repo = spira_config::resolve::derive_home_repo(home_path, &env_map);
    spira_config::resolve::resolve_for_process(home_path, &repo, &env_map).unwrap_or_default()
}

/// `fayth_get`'s own subshell: `. "<fayth-file>" 2>/dev/null; eval "printf '%s' \"\${$var:-$def}\""`.
/// A `.fayth` file is an arbitrary shell fragment (the fixtures use `${SPIRA_SCOPE_LABEL:+...}`
/// parameter expansion); this bridges to the same mechanism `fayth_get` uses rather than
/// writing a second, partial shell-fragment parser (DESIGN.md).
///
/// `home` resolves `SPIRA_CZAR_LABEL`/`SPIRA_GROOMER_LABEL`/`SPIRA_MAECHEN_LABEL` (wave 4.9,
/// sp-k80sa) and sets them explicitly on this subshell's own `Command`: `czar.fayth`/
/// `groomer.fayth`/`maechen.fayth` each reference one by parameter expansion in
/// `FAYTH_LABELS`, and an unset expansion there is silently empty, not an error — the
/// SAME fix `spira-config`'s own `chamber::fayth_get` carries, and for the same scar
/// (round 151: an unexported label read back empty and the persona roster went empty
/// with it).
fn fayth_get(home: &str, fayth_file: &str, var: &str, default: &str) -> String {
    if !Path::new(fayth_file).is_file() {
        return default.to_string();
    }
    let script =
        r#"f="$1"; var="$2"; def="$3"; . "$f" 2>/dev/null; eval "printf '%s' \"\${$var:-\$def}\"""#;
    let resolved = resolved_config(home);
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(script)
        .arg("fayth_get")
        .arg(fayth_file)
        .arg(var)
        .arg(default);
    for k in ["SPIRA_CZAR_LABEL", "SPIRA_GROOMER_LABEL", "SPIRA_MAECHEN_LABEL"] {
        if let Some(v) = resolved.values.get(k) {
            cmd.env(k, v);
        }
    }
    let out = cmd.output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => default.to_string(),
    }
}

/// `SPIRA_HOME`, preferring an explicit override from THIS process's own environment over
/// the `--home` argument (`dirname "$0"`, `bead.sh`'s own location). This is the one place
/// `chamber_dir` ever differed from `home`: several suites (`test-bead-contract.sh`,
/// notably) pin a scratch chamber by exporting `SPIRA_HOME` themselves, distinct from
/// wherever the real `bead.sh` lives on PATH — a fact that survives `exec` on its own
/// (an already-exported var keeps its export attribute across reassignment, and across
/// `exec`, with no help from `bead.sh`), so it needed no re-export even before wave 4.9.
/// `home_arg` is the correct answer only for the UNEXPORTED, no-override, production case
/// — exactly conf.sh's own `SPIRA_HOME="${SPIRA_HOME:-$_spira_conf_here}"` derived default,
/// which is this same `--home` value.
fn chamber_home(home_arg: &str) -> String {
    env::var("SPIRA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| home_arg.to_string())
}

fn chamber_dir(home: &str) -> String {
    format!("{}/chamber", chamber_home(home))
}

/// `fayth_names`: every `*.fayth` basename under `<home>/chamber`, sorted — bash's own
/// glob expansion sorts lexicographically, so this matches it with `Vec::sort`.
fn fayth_names(home: &str) -> Vec<String> {
    let mut names = match std::fs::read_dir(chamber_dir(home)) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("fayth") {
                    p.file_stem().and_then(|s| s.to_str()).map(str::to_string)
                } else {
                    None
                }
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

fn schema_sh(home: &str) -> String {
    format!("{home}/schema.sh")
}

/// `schema.sh type-of <kind>` — `Ok(bd_type)` or `Err(())` (stderr discarded: `bead.sh`
/// prints its own "unknown kind" message, never schema.sh's).
fn schema_type_of(home: &str, kind: &str) -> Result<String, ()> {
    let out = Command::new(schema_sh(home))
        .arg("type-of")
        .arg(kind)
        .output()
        .map_err(|_| ())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(())
    }
}

/// `schema.sh kinds` — passed straight through to stdout by `contract` (bash's own
/// behaviour: the call sits inline between two of `_bead_contract`'s own `printf`s).
fn schema_kinds_passthrough(home: &str) -> String {
    Command::new(schema_sh(home))
        .arg("kinds")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// `schema.sh name <key>` — used for the two names (`scope`, `insight`) `bead.sh file`'s
/// non-work path reads from the live schema rather than an inline bash default, so this
/// bridges rather than duplicating `schema_name`'s own default table.
fn schema_name(home: &str, key: &str) -> String {
    Command::new(schema_sh(home))
        .arg("name")
        .arg(key)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// `aeon_alive <pidfile>`: ordinary Rust, no shell needed — `/proc/<pid>/cmdline` against
/// the `(^|/)aeon( |$)|aeon\.sh` pattern, matching `lib.sh`'s own liveness check.
fn aeon_alive(pidfile: &str) -> bool {
    let pid = match std::fs::read_to_string(pidfile) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return false,
    };
    if pid.is_empty() || !Path::new(&format!("/proc/{pid}")).is_dir() {
        return false;
    }
    let cmdline = std::fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let cmd = cmdline.replace('\0', " ");
    if cmd.contains("aeon.sh") {
        return true;
    }
    cmd.split_whitespace().any(|tok| {
        let base = tok.rsplit('/').next().unwrap_or(tok);
        base == "aeon"
    })
}

fn mail_send(aeon_id: &str, body: &str) {
    // mail, by name on the launcher's PATH (sp-gypjk) — never a constructed
    // "$SPIRA_HOME/mail.sh" path: mail.sh is a compat symlink now (sp-ooh1k), not the
    // tool, and "never construct a path to a Spira tool" is the rule this was breaking.
    let mut child = match Command::new("mail")
        .arg("send")
        .arg(aeon_id)
        .arg("--from")
        .arg("amend <amend@spira>")
        .arg("--subject")
        .arg("Update while you work")
        .env("SPIRA_MAIL_LINT_CONSIDERED", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(body.as_bytes());
    }
    let _ = child.wait();
}

// =========================================================================================
// Repository map / env helpers
// =========================================================================================

/// `SPIRA_REPO_MAP`, resolved in-process (wave 4.9, sp-k80sa) rather than read back out of
/// this binary's own environment — `bead.sh` no longer re-exports it across the `exec`
/// boundary (see `resolved_config`'s own doc).
fn load_repos(home: &str) -> std::collections::BTreeMap<String, spira_config::RepoSection> {
    let path = match resolved_config(home).values.get("SPIRA_REPO_MAP") {
        Some(p) if !p.is_empty() => p.clone(),
        _ => return std::collections::BTreeMap::new(),
    };
    match std::fs::read_to_string(&path) {
        Ok(content) => repos_by_name(&content),
        Err(_) => std::collections::BTreeMap::new(),
    }
}

fn env_default(key: &str, default: &str) -> String {
    env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

// =========================================================================================
// file
// =========================================================================================

fn cmd_file(home: &str, args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("bead: title required");
        return 2;
    }
    let title = args[0].clone();

    let mut for_fayth: Option<String> = None;
    let mut repo: Option<String> = None;
    let mut priority: Option<String> = None;
    let mut body_file: Option<String> = None;
    let mut kind: Option<String> = None;
    let mut express = false;
    let mut json = false;
    let mut parent: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--for" => {
                i += 1;
                for_fayth = args.get(i).cloned();
            }
            "--repo" => {
                i += 1;
                repo = args.get(i).cloned();
            }
            "--priority" | "-p" => {
                i += 1;
                priority = args.get(i).cloned();
            }
            "--body-file" => {
                i += 1;
                body_file = args.get(i).cloned();
            }
            "--kind" => {
                i += 1;
                kind = args.get(i).cloned();
            }
            "--parent" => {
                i += 1;
                parent = args.get(i).cloned();
            }
            "--express" => express = true,
            "--json" => json = true,
            other => {
                eprintln!("bead: unknown option: {other}");
                return 2;
            }
        }
        i += 1;
    }

    let repos = load_repos(home);
    if let Some(r) = &repo {
        if !repos.contains_key(r) {
            let valid = repos.keys().cloned().collect::<Vec<_>>().join(" ");
            let valid = if valid.is_empty() {
                "<map not found>".to_string()
            } else {
                valid
            };
            eprintln!("bead: repo:{r} is not in the repo map; valid keys: {valid}");
            return 2;
        }
    }

    if kind.is_some() && for_fayth.is_some() {
        eprintln!("bead: --kind and --for are mutually exclusive");
        return 2;
    }

    let kind = kind.unwrap_or_else(|| "work".to_string());
    if kind == "gate" {
        eprintln!("bead: kind=gate is a bd-internal type; use bd gate directly");
        return 2;
    }

    let bd_type = match schema_type_of(home, &kind) {
        Ok(t) => t,
        Err(()) => {
            eprintln!("bead: unknown kind: {kind}");
            return 2;
        }
    };
    let express_label = env_default("SPIRA_EXPRESS_LABEL", "express");

    if kind == "work" {
        let for_fayth = match for_fayth {
            Some(f) => f,
            None => {
                eprintln!("bead: --for <persona> required");
                return 2;
            }
        };
        let repo = match repo {
            Some(r) => r,
            None => {
                eprintln!("bead: --repo <name> required");
                return 2;
            }
        };
        let fpath = format!("{}/{}.fayth", chamber_dir(home), for_fayth);
        if !Path::new(&fpath).is_file() {
            eprintln!("bead: no such persona: {for_fayth}");
            return 2;
        }
        let fayth_labels = fayth_get(home, &fpath, "FAYTH_LABELS", "");
        if fayth_labels.is_empty() {
            eprintln!("bead: persona {for_fayth} has no partition labels");
            return 2;
        }

        let lane_override = env::var("SPIRA_BEAD_LANE_OVERRIDE")
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        if let LaneCheck::Refused(msg) = lane_check(&fayth_labels, &repo, &repos, lane_override) {
            eprintln!("{msg}");
            return 2;
        }

        let labels = work_labels(&fayth_labels, &repo, &express_label, express);
        let mut bd_args = s(&["create"]);
        bd_args.push(title);
        bd_args.push("-l".into());
        bd_args.push(labels);
        if let Some(p) = &priority {
            bd_args.push("-p".into());
            bd_args.push(p.clone());
        }
        if let Some(bf) = &body_file {
            bd_args.push("--body-file".into());
            bd_args.push(bf.clone());
        }
        if json {
            bd_args.push("--json".into());
        } else {
            bd_args.push("--silent".into());
        }
        if let Some(p) = &parent {
            bd_args.push("--parent".into());
            bd_args.push(p.clone());
            bd_args.push("--no-inherit-labels".into());
        }
        bdq_status(home, &bd_args)
    } else {
        let scope_label = schema_name(home, "scope");
        let insight_label = if kind == "insight" {
            Some(schema_name(home, "insight"))
        } else {
            None
        };
        let labels = non_work_labels(
            &scope_label,
            insight_label.as_deref(),
            repo.as_deref(),
            &express_label,
            express,
        );

        let mut bd_args = s(&["create"]);
        bd_args.push(title);
        bd_args.push("-l".into());
        bd_args.push(labels);
        bd_args.push("--type".into());
        bd_args.push(bd_type);
        let mut priority = priority;
        if kind == "insight" {
            bd_args.push("--status".into());
            bd_args.push("closed".into());
            if priority.is_none() {
                priority = Some("4".to_string());
            }
        }
        if let Some(p) = &priority {
            bd_args.push("-p".into());
            bd_args.push(p.clone());
        }
        if let Some(bf) = &body_file {
            bd_args.push("--body-file".into());
            bd_args.push(bf.clone());
        }
        if json {
            bd_args.push("--json".into());
        } else {
            bd_args.push("--silent".into());
        }
        if let Some(p) = &parent {
            bd_args.push("--parent".into());
            bd_args.push(p.clone());
            bd_args.push("--no-inherit-labels".into());
        }
        bdq_status(home, &bd_args)
    }
}

// =========================================================================================
// amend
// =========================================================================================

fn cmd_amend(home: &str, args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("bead: amend: id required");
        return 2;
    }
    let id = args[0].clone();
    let mut note: Option<String> = None;
    let mut body_file: Option<String> = None;
    let mut express = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--note" => {
                i += 1;
                note = args.get(i).cloned();
            }
            "--body-file" => {
                i += 1;
                body_file = args.get(i).cloned();
            }
            "--express" => express = true,
            other => {
                eprintln!("bead: amend: unknown option: {other}");
                return 2;
            }
        }
        i += 1;
    }
    if note.is_none() && body_file.is_none() && !express {
        eprintln!("bead: amend: --note, --body-file, or --express required");
        return 2;
    }

    let mut changed = String::new();
    let mut rc = 0;
    if express {
        let express_label = env_default("SPIRA_EXPRESS_LABEL", "express");
        rc |= bdq_status(home, &s(&["label", "add", &id, &express_label]));
        changed.push_str("Marked express.");
    }
    if let Some(n) = &note {
        rc |= bdq_status(
            home,
            &s(&["note"])
                .into_iter()
                .chain([id.clone(), n.clone()])
                .collect::<Vec<_>>(),
        );
        if !changed.is_empty() {
            changed.push_str("\n\n");
        }
        changed.push_str(n);
    }
    if let Some(bf) = &body_file {
        let mut argv = s(&["update"]);
        argv.push(id.clone());
        argv.push("--body-file".into());
        argv.push(bf.clone());
        rc |= bdq_status(home, &argv);
        if !changed.is_empty() {
            changed.push_str("\n\n");
        }
        changed.push_str("Description updated.");
    }

    notify_live_aeon(&id, &changed);
    rc
}

/// The aeon-mail notify tail of `amend`: the first `$SPIRA_RUN/aeon-*-<id>.pid` that names a
/// live aeon, provided its mailbox still exists (the aeon may have already exited between
/// the glob and the check — `break`, not `continue`, on a missing mailbox, matching the bash).
fn notify_live_aeon(id: &str, changed: &str) {
    let run = match env::var("SPIRA_RUN") {
        Ok(r) if !r.is_empty() => r,
        _ => return,
    };
    let entries = match std::fs::read_dir(&run) {
        Ok(e) => e,
        Err(_) => return,
    };
    let suffix = format!("-{id}.pid");
    let mut candidates: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("aeon-") && n.ends_with(&suffix))
        .collect();
    candidates.sort();
    for name in candidates {
        let pidfile = format!("{run}/{name}");
        if !aeon_alive(&pidfile) {
            continue;
        }
        let mail = env::var("SPIRA_MAIL").unwrap_or_default();
        let mailbox_new = format!("{mail}/aeon-{id}/new");
        if !Path::new(&mailbox_new).is_dir() {
            break;
        }
        mail_send(&format!("aeon-{id}"), changed);
        break;
    }
}

// =========================================================================================
// contract
// =========================================================================================

fn cmd_contract(home: &str) -> i32 {
    println!("PERSONAS");
    for f in fayth_names(home) {
        let fpath = format!("{}/{}.fayth", chamber_dir(home), f);
        let labels = fayth_get(home, &fpath, "FAYTH_LABELS", "");
        println!("{}", persona_line(&f, &labels));
    }
    println!();
    println!("KINDS");
    print!("{}", schema_kinds_passthrough(home));
    println!();
    println!("REPOS");
    print!("{}", repos_section(&load_repos(home)));
    0
}

// =========================================================================================
// lint
// =========================================================================================

fn cmd_lint(home: &str, args: &[String]) -> i32 {
    let ids: Vec<String> = if args.is_empty() || args[0] == "--all" {
        let (_, out) = bdq_capture(home, &s(&["list", "--all", "--limit", "0", "--json"]));
        parse_list_ids(&out)
    } else {
        args.to_vec()
    };

    let personas: Vec<(String, String)> = fayth_names(home)
        .into_iter()
        .map(|f| {
            let fpath = format!("{}/{}.fayth", chamber_dir(home), f);
            let labels = fayth_get(home, &fpath, "FAYTH_LABELS", "");
            (f, labels)
        })
        .collect();
    // NOTE: the scope exclusion here defaults to EMPTY, not "spira" — `_bead_lint` itself
    // reads `${SPIRA_SCOPE_LABEL:-}`, a different default than `file`'s `schema.sh name
    // scope` call. Preserved exactly; see DESIGN.md.
    let scope_for_partitions = env::var("SPIRA_SCOPE_LABEL").unwrap_or_default();
    let partitions = chamber_partitions(&personas, &scope_for_partitions);
    let partitions_joined = partitions.join(" ");

    let no_loop_label = env::var("SPIRA_NO_LOOP_LABEL").unwrap_or_default();
    let ask_label = env::var("SPIRA_ASK_LABEL").unwrap_or_default();
    let incident_label = env_default("SPIRA_INCIDENT_LABEL", "incident");

    let mut n = 0u32;
    let mut bad = 0u32;
    let mut rc = 0;
    let mut label_cache: HashMap<String, Vec<String>> = HashMap::new();

    for id in &ids {
        if id.is_empty() {
            continue;
        }
        n += 1;
        let (code, out) = bdq_capture(
            home,
            &["show".to_string(), id.clone(), "--json".to_string()],
        );
        if code != 0 || out.trim().is_empty() {
            eprintln!("bead: {id}: unreadable (bd show failed)");
            bad += 1;
            rc = 1;
            continue;
        }
        let row = match parse_show_row(&out) {
            Some(r) => r,
            None => {
                eprintln!("bead: {id}: unreadable (bd show failed)");
                bad += 1;
                rc = 1;
                continue;
            }
        };
        let labels_joined = row.labels.join(" ");

        if let Some(braw) = branch_label(&row.labels) {
            let cand = branch_candidate(braw);
            if !cand.is_empty() && cand != id {
                let (scode, _) = bdq_capture(home, &s(&["show", cand, "--json"]));
                if scode == 0 {
                    eprintln!("bead: {id}: branch: label names {cand}, not itself");
                    bad += 1;
                    rc = 1;
                }
            }
        }

        let (_, deps_out) =
            bdq_capture(home, &s(&["dep", "list", id, "--type", "blocks", "--json"]));
        let blocks_targets = parse_blocks_targets(&deps_out);
        for oid in &blocks_targets {
            label_cache.entry(oid.clone()).or_insert_with(|| {
                let (_, oout) = bdq_capture(home, &s(&["show", oid, "--json"]));
                parse_show_row(&oout).map(|r| r.labels).unwrap_or_default()
            });
        }

        if !ask_label.is_empty() && row.labels.iter().any(|l| l == &ask_label) {
            for oid in &blocks_targets {
                if label_cache
                    .get(oid)
                    .map(|ls| ls.iter().any(|l| l == &ask_label))
                    .unwrap_or(false)
                {
                    eprintln!(
                        "bead: {id}: blocks edge to ask-labelled {oid} (asks must not block each other; use bd dep relate)"
                    );
                    bad += 1;
                    rc = 1;
                }
            }
        }
        for oid in &blocks_targets {
            if label_cache
                .get(oid)
                .map(|ls| ls.iter().any(|l| l == &incident_label))
                .unwrap_or(false)
            {
                eprintln!(
                    "bead: {id}: blocks edge to incident-labelled {oid} (alarms must not block work; use bd dep relate)"
                );
                bad += 1;
                rc = 1;
            }
        }

        let (_, lines) = lint_judge(
            &labels_joined,
            &row.status,
            &row.issue_type,
            &partitions_joined,
            &no_loop_label,
        );
        for line in lines {
            eprintln!("bead: {id}: {line}");
            bad += 1;
            rc = 1;
        }
    }

    if bad == 0 {
        println!("bead: {n} bead(s) checked, ok");
    }
    rc
}

// =========================================================================================
// dep add
// =========================================================================================

fn cmd_dep_add(home: &str, args: &[String]) -> i32 {
    let mut id: Option<String> = None;
    let mut depid: Option<String> = None;
    let mut dep_type: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut pos_n = 0;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--type" | "-t" => {
                i += 1;
                dep_type = args.get(i).cloned();
            }
            "--blocked-by" => {
                i += 1;
                depid = args.get(i).cloned();
            }
            "--depends-on" => {
                i += 1;
                depid = args.get(i).cloned();
            }
            "--file" => {
                eprintln!("bead: dep add: --file bulk wiring is not wrapped; use bd dep add --file directly");
                return 2;
            }
            a if a.starts_with('-') => rest.push(a.to_string()),
            a => match pos_n {
                0 => {
                    id = Some(a.to_string());
                    pos_n = 1;
                }
                1 => {
                    if depid.is_none() {
                        depid = Some(a.to_string());
                    }
                    pos_n = 2;
                }
                _ => rest.push(a.to_string()),
            },
        }
        i += 1;
    }

    let (id, depid) = match (id, depid) {
        (Some(i), Some(d)) => (i, d),
        _ => {
            eprintln!("usage: bead.sh dep add <id> <depends-on-id> [--type <type>]");
            return 2;
        }
    };

    if is_blocks_type(dep_type.as_deref()) {
        let incident_label = env_default("SPIRA_INCIDENT_LABEL", "incident");
        let (_, out) = bdq_capture(home, &s(&["show", &depid, "--json"]));
        let target_labels = parse_show_row(&out).map(|r| r.labels).unwrap_or_default();
        if target_labels.iter().any(|l| l == &incident_label) {
            eprintln!("{}", incident_blocks_refusal(&id, &depid, &incident_label));
            return 1;
        }
    }

    let mut call = s(&["dep", "add"]);
    call.push(id);
    call.push(depid);
    if let Some(t) = &dep_type {
        call.push("--type".into());
        call.push(t.clone());
    }
    call.extend(rest);
    bdq_status(home, &call)
}

#[cfg(test)]
mod tests {
    use super::aeon_alive;
    use std::io::Write;

    #[test]
    fn aeon_alive_false_on_missing_pidfile() {
        assert!(!aeon_alive("/nonexistent/pidfile"));
    }

    #[test]
    fn aeon_alive_false_on_dead_pid() {
        let dir = testkit::TempDir::new("bead-aeon-alive");
        let pf = dir.join("x.pid");
        // pid 1 might be real (init) but its cmdline will never match "aeon"; use a pid
        // that (almost certainly) does not exist instead, to exercise the /proc check.
        std::fs::write(&pf, "999999999\n").unwrap();
        assert!(!aeon_alive(pf.to_str().unwrap()));
    }

    #[test]
    fn aeon_alive_true_on_self_if_argv0_matches() {
        // This process's own cmdline is the test binary, not "aeon" — negative check that
        // a live, unrelated process is correctly NOT reported as an aeon.
        let dir = testkit::TempDir::new("bead-aeon-alive-self");
        let pf = dir.join("self.pid");
        let mut f = std::fs::File::create(&pf).unwrap();
        write!(f, "{}", std::process::id()).unwrap();
        drop(f);
        assert!(!aeon_alive(pf.to_str().unwrap()));
    }
}
