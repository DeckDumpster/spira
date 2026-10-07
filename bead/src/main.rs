//! `bead` — see DESIGN.md. `spira/bead.sh` is the shim every caller names; it `exec`s into
//! this binary by bare name on the release PATH, passing its own directory as `--home`
//! (the same convention `gate.sh` established).

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use bead::claimdesc::notify_live_aeon;
use bead::{
    branch_candidate, branch_label, chamber_partitions, incident_blocks_refusal, is_blocks_type,
    awaits_dispatch, lane_check, lint_judge, parse_created_id, PARTITION_CHECK_TYPES, non_work_labels, parse_blocks_targets, parse_list_ids, parse_show_row,
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
        "judge-create" => cmd_judge_create(&home, &rest),
        "event" => cmd_event(&rest),
        "dep" => match rest.first().map(String::as_str) {
            Some("add") => cmd_dep_add(&home, &rest[1..]),
            Some("remove") => cmd_dep_remove(&home, &rest[1..]),
            _ => {
                eprintln!("usage: bead.sh dep add <id> <depends-on-id> [--type <type>]\n       bead.sh dep remove <id> <depends-on-id>");
                2
            }
        },
        _ => {
            eprintln!(
                "usage: bead.sh file \"<title>\" --for <persona> --repo <name> [--priority N] [--body-file F] [--express] [--submitted] [--json]\n       bead.sh file \"<title>\" --kind <kind> [--repo <name>] [--priority N] [--body-file F] [--express] [--submitted] [--json]\n       bead.sh amend <id> [--note \"<text>\"] [--body-file F] [--express]\n       bead.sh dep add <id> <depends-on-id> [--type <type>]\n       bead.sh dep remove <id> <depends-on-id>\n       bead.sh lint [--all|<id>...]\n       bead.sh contract\n       bead event <kind> <target|-> <title> [detail]"
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
    // batch-job: runs a gate, build or forge script that takes as long as its work
    Command::new("bash")
        .arg("-c")
        .arg(BDQ_SCRIPT)
        .arg("bdq")
        .arg(home)
        .args(args)
        // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own
        // release's bin/+spira/ on the CHILD's PATH, never only inherited.
        .envs(spira_config::release_env::child_path_env_for_process())
        .status()
        .map(|s| s.code().unwrap_or(1))
        .unwrap_or(127)
}

/// Runs `bdq` capturing stdout, discarding stderr (`2>/dev/null`, matching every read call
/// `bead.sh`'s own sweep made).
fn bdq_capture(home: &str, args: &[String]) -> (i32, String) {
    bdq_capture_with(home, args, Stdio::null())
}

fn bdq_capture_with(home: &str, args: &[String], stderr: Stdio) -> (i32, String) {
    // batch-job: runs a gate, build or forge script that takes as long as its work
    let out = Command::new("bash")
        .arg("-c")
        .arg(BDQ_SCRIPT)
        .arg("bdq")
        .arg(home)
        .args(args)
        // law-a-binary-resolves-the-config-it-reads (sp-kgzql): this binary's own
        // release's bin/+spira/ on the CHILD's PATH, never only inherited.
        .envs(spira_config::release_env::child_path_env_for_process())
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(stderr)
        .output();
    match out {
        Ok(o) => (
            o.status.code().unwrap_or(1),
            String::from_utf8_lossy(&o.stdout).into_owned(),
        ),
        Err(_) => (127, String::new()),
    }
}

/// Whether `id` carries the incident label. A bd parent cannot be blocked on its own child,
/// so a remedy filed under an incident is wired as a plain blocks edge instead of a child.
fn is_incident(home: &str, id: &str) -> bool {
    let incident_label = cfg_label("SPIRA_INCIDENT_LABEL");
    let (_, out) = bdq_capture(home, &s(&["show", id, "--json"]));
    parse_show_row(&out).map(|r| r.labels.contains(&incident_label)).unwrap_or(false)
}

/// `bdq_status` for a create: stdout is passed through unchanged, and when `incident` is
/// set that incident is wired to block on the new bead in the same step
/// (law-a-bug-with-a-fix-in-flight-depends-on-it). Failing to wire it fails the filing.
fn bdq_create(home: &str, args: &[String], incident: Option<&str>) -> i32 {
    let Some(incident) = incident else { return bdq_status(home, args) };
    let (code, stdout) = bdq_capture_with(home, args, Stdio::inherit());
    print!("{stdout}");
    if code != 0 {
        return code;
    }
    let Some(new_id) = parse_created_id(&stdout) else {
        eprintln!("bead: file: created a remedy for incident {incident} but could not read its id; add the edge: bd dep add {incident} <id>");
        return 1;
    };
    let rc = bdq_status(home, &s(&["dep", "add", incident, &new_id, "--type", "blocks"]));
    if rc != 0 {
        eprintln!("bead: file: {new_id} filed but the blocks edge {incident} -> {new_id} failed; add it: bd dep add {incident} {new_id}");
    }
    rc
}

fn s(strs: &[&str]) -> Vec<String> {
    strs.iter().map(|s| s.to_string()).collect()
}

/// `FAYTH_LABELS`, plainly evaluated — delegates to `spira_config::chamber::fayth_get`
/// (sp-xsnid retired this crate's OWN hand-rolled bash-bridge, which carried an
/// independently-drifted three-key label overlay — `SPIRA_CZAR_LABEL`/
/// `SPIRA_GROOMER_LABEL`/`SPIRA_MAECHEN_LABEL` only, never `SPIRA_INCIDENT_LABEL`/
/// `SPIRA_SPIKE_LABEL`/`SPIRA_PLAN_LABEL`/`SPIRA_SCOPE_LABEL`/`SPIRA_ASK_LABEL`/
/// `SPIRA_CI_LABEL` — a SECOND hand-curated list with its own gaps, on top of
/// `spira-config`'s own five-key one). For REPORTING call sites only (`bead contract`,
/// `bead lint`): an empty result here just means the report shows an empty partition for
/// that persona, never a claim decision, so it never needs [`fayth_predicate`]'s
/// fail-closed refusal. `home` is [`chamber_home`]'s own `$SPIRA_HOME`-override
/// precedence, matching every other chamber read in this file.
fn fayth_label(home: &str, fayth: &str) -> String {
    spira_config::chamber::fayth_get(Path::new(&chamber_home(home)), fayth, "FAYTH_LABELS", "")
}

/// `FAYTH_LABELS`/`FAYTH_EXCLUDE_LABELS`, fully resolved and FAIL-CLOSED — delegates to
/// `spira_config::chamber::fayth_predicate`, the one shared evaluator every
/// ready-counting/claiming/FILING caller now goes through (sp-xsnid). For the one call
/// site that decides what a bead gets FILED as (`bead file --for`): a predicate that
/// would otherwise widen to the whole ready queue must refuse the filing outright, never
/// hand back an empty label silently.
fn fayth_predicate(home: &str, fayth: &str) -> Result<spira_config::chamber::Predicate, spira_config::chamber::Refusal> {
    spira_config::chamber::fayth_predicate(Path::new(&chamber_home(home)), fayth)
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
    spira_config::chamber::chamber_dir(Path::new(&chamber_home(home))).to_string_lossy().into_owned()
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
    let out = spira_config::bounded::bounded(schema_sh(home))
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
    spira_config::bounded::bounded(schema_sh(home))
        .arg("kinds")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// `schema.sh name <key>` — used for the two names (`scope`, `insight`) `bead.sh file`'s
/// non-work path reads from the live schema rather than an inline bash default, so this
/// bridges rather than duplicating `schema_name`'s own default table.
fn schema_name(home: &str, key: &str) -> String {
    spira_config::bounded::bounded(schema_sh(home))
        .arg("name")
        .arg(key)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

// =========================================================================================
// Repository map / env helpers
// =========================================================================================

/// `SPIRA_REPO_MAP` through `cfg` (per Ryan 2026-10-05: one source of config) — the config
/// file `cfg` reads is the same whatever `--home` says; `--home` only ever selects
/// chamber/registry PATHS (`chamber_home`/`chamber_dir`/`fayth_names`), never which
/// config file is in force.
fn load_repos() -> std::collections::BTreeMap<String, spira_config::RepoSection> {
    let path = cfg_label("SPIRA_REPO_MAP");
    if path.is_empty() {
        return std::collections::BTreeMap::new();
    }
    match std::fs::read_to_string(&path) {
        Ok(content) => repos_by_name(&content),
        Err(_) => std::collections::BTreeMap::new(),
    }
}

/// One source of config (per Ryan 2026-10-05): a registered key's value, through
/// `spira_config::process::cfg`, resolved from `$SPIRA_TOML`. Several of these keys treat
/// a resolved empty string as a legitimate, documented value (SPIRA_SCOPE_LABEL's "no
/// scope exclusion", in particular) — that passes straight through unchanged. Only an
/// actual resolution failure refuses, naming the key; it is never silently substituted.
fn cfg_label(key: &str) -> String {
    match spira_config::process::cfg(key) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("bead: {e}");
            std::process::exit(1);
        }
    }
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
    let mut submitted = false;
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
            "--submitted" => submitted = true,
            "--json" => json = true,
            other => {
                eprintln!("bead: unknown option: {other}");
                return 2;
            }
        }
        i += 1;
    }

    let incident_parent = match &parent {
        Some(p) if is_incident(home, p) => parent.take(),
        _ => None,
    };

    let repos = load_repos();
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
    let express_label = cfg_label("SPIRA_EXPRESS_LABEL");

    if submitted && kind != "work" {
        eprintln!("bead: --submitted applies only to work beads");
        return 2;
    }

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
        let fayth_labels = match fayth_predicate(home, &for_fayth) {
            Ok(p) => p.labels,
            Err(e) => {
                eprintln!("bead: {e}");
                return 2;
            }
        };
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

        let mut labels = work_labels(&fayth_labels, &repo, &express_label, express);
        if submitted {
            labels.push(',');
            labels.push_str(&cfg_label("SPIRA_SUBMITTED_LABEL"));
        }
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
        bdq_create(home, &bd_args, incident_parent.as_deref())
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
        bdq_create(home, &bd_args, incident_parent.as_deref())
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
        let express_label = cfg_label("SPIRA_EXPRESS_LABEL");
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
        argv.push(bead::claimdesc::FORCE_FLAG.into());
        argv.push("bead.sh amend".into());
        rc |= bdq_status(home, &argv);
        if !changed.is_empty() {
            changed.push_str("\n\n");
        }
        changed.push_str("Description updated.");
    }

    // Best-effort notification, same as bdq's own `acknowledge_forced_edit`: the real
    // amend above has already landed, so a config-resolution failure here must not abort
    // and lose `rc` — `notify_live_aeon` already treats an empty SPIRA_RUN/SPIRA_MAIL as
    // "nothing to notify".
    let run = spira_config::process::cfg("SPIRA_RUN").unwrap_or_default();
    let mail = spira_config::process::cfg("SPIRA_MAIL").unwrap_or_default();
    notify_live_aeon(&id, &changed, &run, &mail);
    rc
}

// =========================================================================================
// contract
// =========================================================================================

fn cmd_contract(home: &str) -> i32 {
    println!("PERSONAS");
    for f in fayth_names(home) {
        let labels = fayth_label(home, &f);
        println!("{}", persona_line(&f, &labels));
    }
    println!();
    println!("KINDS");
    print!("{}", schema_kinds_passthrough(home));
    println!();
    println!("REPOS");
    print!("{}", repos_section(&load_repos()));
    0
}

// =========================================================================================
// event — sp-ogu8x, wave 4.24, family Z. `spira/lib.sh`'s `spira_event` is now a one-line
// shim onto this subcommand (see DESIGN.md "event"); every other caller (the aeon/gate-check/
// landing-pass/queue/sentinel seams) still types the bash function name unchanged.
// =========================================================================================

fn cmd_event(args: &[String]) -> i32 {
    // Mirrors the bash's own positional defaults: `target="${2:--}"`, so a caller that
    // omits it gets "-" (a plan-level event), not an empty string.
    let kind = args.first().cloned().unwrap_or_default();
    let target = args.get(1).cloned().unwrap_or_else(|| "-".to_string());
    let title = args.get(2).cloned().unwrap_or_default();
    let detail = args.get(3).cloned().unwrap_or_default();

    // `dir="$SPIRA_RUN/events"` in the bash: an unset SPIRA_RUN concatenates to a literal
    // leading slash, which every real caller's conf.sh-derived environment avoids by
    // always setting SPIRA_RUN. Matched here as a plain string join, not `Path::join` on an
    // empty base (which would silently go relative instead) — see DESIGN.md "event".
    // Unset or empty SPIRA_RUN is refused by name (sp-0c1wz): the old join made a relative
    // path and the event vanished with rc 0. SPIRA_RUN is a registered key, so this now
    // reads it through `cfg` rather than the process environment directly.
    let run_dir = match spira_config::process::cfg("SPIRA_RUN") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        Ok(_) => {
            eprintln!("spira_event: SPIRA_RUN resolved empty — refusing rather than drop the event");
            return 2;
        }
        Err(e) => {
            eprintln!("spira_event: {e}");
            return 2;
        }
    };
    // SPIRA_EVENT_COOLDOWN / SPIRA_NOW are not registered config keys (spira/conf.d has no
    // entry for either) — left as plain env reads with their existing defaults.
    let cooldown: i64 = env::var("SPIRA_EVENT_COOLDOWN").ok().and_then(|v| v.parse().ok()).unwrap_or(3600);
    let now: i64 = env::var("SPIRA_NOW").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    });

    match bead::event::emit(&run_dir, cooldown, now, &kind, &target, &title, &detail) {
        Ok(()) => 0,
        Err(()) => 1,
    }
}

// =========================================================================================
// lint
// =========================================================================================

/// `bead judge-create <labels-csv> [type]` — the lint judge applied to a bead about to be
/// created open. Exit 1 with the partition list on stderr when the bead would be unclaimable;
/// the aeon fence calls this so a raw `bd create` is refused by the same rule `file` obeys.
fn cmd_judge_create(home: &str, args: &[String]) -> i32 {
    let labels_csv = args.first().cloned().unwrap_or_default();
    let ty = args.get(1).map(String::as_str).unwrap_or("task");
    let labels = labels_csv.replace(',', " ");
    let scope = cfg_label("SPIRA_SCOPE_LABEL");
    if !scope.is_empty() && !labels.split_whitespace().any(|l| l == scope) {
        return 0;
    }
    let personas: Vec<(String, String)> = fayth_names(home)
        .into_iter()
        .map(|f| {
            let l = fayth_label(home, &f);
            (f, l)
        })
        .collect();
    let partitions = chamber_partitions(&personas, &scope);
    let no_loop = cfg_label("SPIRA_NO_LOOP_LABEL");
    // A bead being filed is about to wait for dispatch.
    let (_, out) = lint_judge(&labels, true, ty, &partitions.join(" "), &no_loop);
    if out.iter().any(|l| l.starts_with("no partition label")) {
        eprintln!(
            "{}; add one of: {}",
            out.iter().find(|l| l.starts_with("no partition label")).unwrap(),
            partitions.join(", ")
        );
        return 1;
    }
    0
}

/// Finished, by the lifecycle machine's own judgement (law-the-lifecycle-is-the-only-authority-on-
/// bead-state): never bd's status. A bead the machine has no row for, or a state it cannot read,
/// is treated as not finished, so the incident checks still run.
fn lc_done(lc: &mut Option<Result<HashMap<String, spira_config::lc_state::Row>, String>>, id: &str) -> bool {
    match lc.get_or_insert_with(|| spira_config::lc_state::list().map(spira_config::lc_state::index)) {
        Ok(rows) => rows.get(id).is_some_and(|r| r.terminal()),
        Err(_) => false,
    }
}

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
            let labels = fayth_label(home, &f);
            (f, labels)
        })
        .collect();
    // NOTE: the scope exclusion here defaults to EMPTY, not "spira" — `_bead_lint` itself
    // reads `${SPIRA_SCOPE_LABEL:-}`, a different default than `file`'s `schema.sh name
    // scope` call. Preserved exactly; see DESIGN.md.
    let scope_for_partitions = cfg_label("SPIRA_SCOPE_LABEL");
    let partitions = chamber_partitions(&personas, &scope_for_partitions);
    let partitions_joined = partitions.join(" ");

    let no_loop_label = cfg_label("SPIRA_NO_LOOP_LABEL");
    let ask_label = cfg_label("SPIRA_ASK_LABEL");
    let incident_label = cfg_label("SPIRA_INCIDENT_LABEL");

    let mut n = 0u32;
    let mut bad = 0u32;
    let mut rc = 0;
    let mut label_cache: HashMap<String, Vec<String>> = HashMap::new();
    // The lifecycle rows, read once on the first work bead that needs one (sp-mve9i: whether a
    // bead awaits dispatch is the machine's answer, not bd's status).
    let mut lc: Option<Result<HashMap<String, spira_config::lc_state::Row>, String>> = None;

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

        let awaits = if PARTITION_CHECK_TYPES.contains(&row.issue_type.as_str()) {
            let first = lc.is_none();
            match lc.get_or_insert_with(|| spira_config::lc_state::list().map(spira_config::lc_state::index)) {
                Ok(rows) => awaits_dispatch(rows.get(id.as_str())),
                Err(e) => {
                    // A lint that cannot read the state cannot pass the bead (law-a-control-
                    // that-cannot-check-must-refuse); said once, counted per bead.
                    if first {
                        eprintln!("bead: lifecycle state unreadable ({e}) — the partition check cannot be judged");
                    }
                    bad += 1;
                    rc = 1;
                    false
                }
            }
        } else {
            false
        };
        if row.labels.iter().any(|l| l == &incident_label) && !lc_done(&mut lc, id) {
            let (_, rel_out) =
                bdq_capture(home, &s(&["dep", "list", id, "--type", "relates-to", "--json"]));
            for oid in parse_blocks_targets(&rel_out) {
                if blocks_targets.contains(&oid) {
                    continue;
                }
                let (_, oout) = bdq_capture(home, &s(&["show", &oid, "--json"]));
                let Some(orow) = parse_show_row(&oout) else { continue };
                if lc_done(&mut lc, &oid) || orow.labels.iter().any(|l| l == &incident_label) {
                    continue;
                }
                eprintln!(
                    "bead: {id}: open remedy {oid} is linked relates-to only (an incident with a fix in flight must block on it; use bd dep add {id} {oid})"
                );
                bad += 1;
                rc = 1;
            }
        }

        let (_, lines) = lint_judge(
            &labels_joined,
            awaits,
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
        let incident_label = cfg_label("SPIRA_INCIDENT_LABEL");
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

// =========================================================================================
// dep remove
// =========================================================================================

fn cmd_dep_remove(home: &str, args: &[String]) -> i32 {
    let (id, depid) = match args {
        [i, d] if !i.starts_with('-') && !d.starts_with('-') => (i.clone(), d.clone()),
        _ => {
            eprintln!("usage: bead.sh dep remove <id> <depends-on-id>");
            return 2;
        }
    };

    for b in [&id, &depid] {
        let (_, out) = bdq_capture(home, &s(&["show", b, "--json"]));
        if parse_show_row(&out).is_none() {
            eprintln!("bead: dep remove: refusing — unknown bead {b}");
            return 1;
        }
    }

    let (_, out) = bdq_capture(home, &s(&["dep", "list", &id, "--json"]));
    if !parse_blocks_targets(&out).iter().any(|t| t == &depid) {
        eprintln!("bead: dep remove: refusing — {id} has no dependency edge on {depid}");
        return 1;
    }

    let rc = bdq_status(home, &s(&["dep", "remove", &id, &depid]));
    if rc == 0 {
        println!("removed edge: {id} -> {depid}");
    }
    rc
}
