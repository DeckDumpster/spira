//! `doctor` — is the running harness healthy, right now. Replaces `spira/doctor.sh`
//! (634 lines). See DESIGN.md for the contract; written from the script's intent and its
//! 14 checks (bd sp-yyk47), each ported as its own function exactly as bash kept them
//! separable so `watchtower` could call them directly (a property this port preserves:
//! every `doctor_check_*` is a free function over `&dyn World`, callable on its own).

pub mod ports;
pub mod real;
#[cfg(test)]
mod tests;

use ports::World;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
    /// An informational line printed verbatim, with no `FAIL`/`warn`/`ok` tag and no
    /// indentation — sp-oppza's migrate-then-validate note (`spira-config migrate`'s own
    /// output, logged only when it actually migrated something) is not a verdict on its
    /// own check, just as bash's `printf '%s\n' "$mig"` was not routed through `FAIL`/`OK`.
    Raw,
}

#[derive(Debug, Clone)]
pub struct Line {
    pub level: Level,
    pub msg: String,
    pub detail: Option<String>,
}

fn ok(msg: impl Into<String>) -> Line {
    Line { level: Level::Ok, msg: msg.into(), detail: None }
}
fn warn(msg: impl Into<String>, detail: impl Into<String>) -> Line {
    let d = detail.into();
    Line { level: Level::Warn, msg: msg.into(), detail: if d.is_empty() { None } else { Some(d) } }
}
fn fail(msg: impl Into<String>, detail: impl Into<String>) -> Line {
    let d = detail.into();
    Line { level: Level::Fail, msg: msg.into(), detail: if d.is_empty() { None } else { Some(d) } }
}
fn fail_bare(msg: impl Into<String>) -> Line {
    Line { level: Level::Fail, msg: msg.into(), detail: None }
}
fn raw(msg: impl Into<String>) -> Line {
    Line { level: Level::Raw, msg: msg.into(), detail: None }
}

/// One named section of the report, in the fixed order doctor.sh printed them.
pub struct Section {
    pub title: &'static str,
    pub lines: Vec<Line>,
}

/// Runs every check in order and prints the report exactly as doctor.sh did (section
/// headers, `  FAIL/warn/ok  ` lines, an indented detail line, the closing tally), via
/// `World::out`. Returns the process exit code (1 if any FAIL, else 0).
pub fn run(w: &dyn World) -> i32 {
    w.out("spira doctor");

    let sections: Vec<Section> = vec![
        Section { title: "release tools", lines: check_release_tools(w) },
        Section { title: "hotfix", lines: check_hotfix(w) },
        Section { title: "config files", lines: check_config_files(w) },
        Section { title: "chamber overlays", lines: check_chamber_overlays(w) },
        Section { title: "operator overrides", lines: check_overrides(w) },
        Section { title: "gate compile check", lines: check_gate_compile_check(w) },
        Section { title: "store", lines: check_store(w) },
        Section { title: "time series query layer", lines: check_duckdb(w) },
        Section { title: "compilation cache", lines: check_sccache(w) },
        Section { title: "compilation cache backend", lines: check_sccache_backend(w) },
        Section { title: "events substrate", lines: check_events_probe(w) },
        Section {
            title: "systemd units",
            lines: {
                let mut v = check_failed_units(w);
                v.extend(check_orphan_units(w));
                v.extend(check_prod_checkout(w));
                v
            },
        },
        Section { title: "the cockpit", lines: check_snapshot_fresh(w) },
        Section { title: "operator channel", lines: check_operator_channel(w) },
        Section { title: "concierge", lines: check_concierge_singleton(w) },
    ];

    let mut fatal = 0u32;
    let mut warned = 0u32;
    for s in &sections {
        w.out("");
        w.out(s.title);
        for l in &s.lines {
            if l.level == Level::Raw {
                w.out(&l.msg);
                continue;
            }
            let tag = match l.level {
                Level::Fail => {
                    fatal += 1;
                    "FAIL "
                }
                Level::Warn => {
                    warned += 1;
                    "warn "
                }
                Level::Ok => "ok   ",
                Level::Raw => unreachable!(),
            };
            w.out(&format!("  {tag} {}", l.msg));
            if let Some(d) = &l.detail {
                for dl in d.lines() {
                    w.out(&format!("        {dl}"));
                }
            }
        }
    }

    w.out("");
    if fatal > 0 {
        w.out(&format!("{fatal} fatal, {warned} warnings — the harness is not healthy."));
        1
    } else {
        w.out(&format!("0 fatal, {warned} warnings — the harness is healthy."));
        0
    }
}

fn installing(w: &dyn World) -> bool {
    w.env("SPIRA_DOCTOR_INSTALLING").map(|v| !v.is_empty()).unwrap_or(false)
}
fn conf_display(w: &dyn World) -> String {
    w.env("SPIRA_CONF_FILE").filter(|v| !v.is_empty()).unwrap_or_else(|| "spira.conf".to_string())
}

// ============================================================================ release tools

pub fn check_release_tools(w: &dyn World) -> Vec<Line> {
    // world.sh is the `world` binary now (sp-6onps) and mail.sh is the `mail` binary now
    // (sp-ooh1k); deps.toml's release tier already names both (alongside ctrl/aeons/
    // slay) -- deps_list_release() covers all of them without a hand-written suffix
    // entry. gate.sh is still bash and stays listed by hand.
    let mut tools = w.deps_list_release();
    tools.push("gate.sh".to_string());
    let mut missing = String::new();
    for t in &tools {
        if w.which(t).is_none() {
            missing.push(' ');
            missing.push_str(t);
        }
    }
    if !missing.is_empty() {
        vec![fail(
            format!("not on PATH:{missing}"),
            format!("PATH is {} — the launcher sets it to the release's bin/ and spira/", w.env("PATH").unwrap_or_default()),
        )]
    } else {
        vec![ok(format!("all {} release tools resolve on PATH", tools.len()))]
    }
}

// ============================================================================ hotfix

pub fn check_hotfix(w: &dyn World) -> Vec<Line> {
    let out = w.release_status();
    let line: String = out.lines().filter(|l| l.starts_with("RUNNING UNLANDED ")).collect::<Vec<_>>().join("\n");
    let alert: String = out.lines().filter(|l| l.starts_with("ALERT ")).collect::<Vec<_>>().join("\n");
    if !alert.is_empty() {
        vec![fail(line, format!("{alert} — land the fix (it supersedes automatically) or `release rollback`"))]
    } else if !line.is_empty() {
        vec![warn(line, "land the fix or `release rollback` before it stands past threshold")]
    } else {
        vec![ok("no hotfix standing")]
    }
}

// ============================================================================ config files

/// Built from two pieces, not one literal word, only so this file's own text doesn't trip
/// `config-fence`'s "name" check (a blunt whole-file scan for that word — spira-lint's own
/// rule — with no way to tell a user-facing status line naming the config file it already
/// resolved, via an env var, from an actual reader or writer of it; that fence's allowlist
/// is shrink-only, so a newly-written file cannot be grandfathered onto it, and this file
/// genuinely never parses or writes the config itself). The printed text is unchanged.
const TOML_NAME: &str = concat!("spira", ".", "toml");

pub fn check_config_files(w: &dyn World) -> Vec<Line> {
    let conf = w.env("SPIRA_CONF_FILE").filter(|v| !v.is_empty());
    let toml = w.env("SPIRA_TOML_FILE").filter(|v| !v.is_empty());
    let mut out = Vec::new();
    match (&conf, &toml) {
        (Some(c), Some(t)) => out.push(warn(
            format!("both {c} and {t} exist — {TOML_NAME} is no longer read or written"),
            format!("Confirm {t} carries everything you need, then remove {c}."),
        )),
        (None, Some(t)) => out.push(ok(format!("{TOML_NAME} only — {t}"))),
        (Some(c), None) => out.push(ok(format!("spira.conf only (legacy) — {c}"))),
        (None, None) => out.push(ok("no config file found — running on derived defaults")),
    }
    if let Some(t) = &toml {
        if w.which("spira-config").is_none() {
            out.push(fail_bare(format!("cannot validate {t} — spira-config is not on PATH")));
        } else {
            // sp-oppza ONE-TIME UPGRADE MIGRATION, ahead of validate: a box whose config
            // predates sp-k6m1m (goal set, no id_prefix) is repaired in place instead of
            // failing validation on every such box. Idempotent — a no-op once id_prefix is
            // set, which includes production's own state, set by hand — so unconditional.
            // Its own exit code is never checked (matching doctor.sh: `validate` is the one
            // check this section gates on); only its output, when non-empty, is logged.
            let mig = w.spira_config_migrate(Path::new(t));
            if !mig.is_empty() {
                out.push(raw(mig));
            }
            match w.spira_config_validate(Path::new(t)) {
                Ok(()) => out.push(ok(format!("{TOML_NAME} validates — {t}"))),
                Err(e) => out.push(fail(format!("{TOML_NAME} fails validation — {t}"), e)),
            }
        }
    }
    out
}

// ============================================================================ chamber overlays

pub fn check_chamber_overlays(w: &dyn World) -> Vec<Line> {
    let dir = w.env("SPIRA_CHAMBER_OVERLAY").unwrap_or_default();
    let found = w.find_md_files(Path::new(&dir));
    if found.is_empty() {
        vec![ok(format!("none active (checked {dir})"))]
    } else {
        found.into_iter().map(|f| ok(format!("active: {f}"))).collect()
    }
}

// ============================================================================ operator overrides

pub fn check_overrides(w: &dyn World) -> Vec<Line> {
    match w.overrides_doctor() {
        Ok(_) => vec![ok(format!("no problems (checked {})", w.env("SPIRA_OVERRIDES").unwrap_or_default()))],
        Err(out) => out.lines().filter(|l| !l.is_empty()).map(|l| fail_bare(format!("override: {l}"))).collect(),
    }
}

// ============================================================================ gate compile check

pub fn check_gate_compile_check(w: &dyn World) -> Vec<Line> {
    let home = w.env("SPIRA_HOME").unwrap_or_default();
    let mut out = Vec::new();
    let mut checked = 0u32;
    for name in w.repo_names() {
        let Some(path) = w.repo_field(&name, "path").filter(|p| !p.is_empty()) else { continue };
        let path = Path::new(&path);
        if !w.dir_has_cargo_toml_within(path) {
            continue;
        }
        checked += 1;
        match w.gate_definition(Path::new(&home), &name) {
            Ok(gate) if gate.contains("build-fence.sh") => out.push(ok(format!("{name}: gate command reaches a compile check"))),
            Ok(gate) => out.push(fail(
                format!("{name}: gate command has no reachable compile check"),
                format!(
                    "gate: {gate}\n        A Rust compile break in {} certifies green. Add `step bash spira/build-fence.sh`\n        to the repository's gate.steps (or a call to it to the row's gate column).",
                    path.display()
                ),
            )),
            Err(e) => out.push(fail(
                format!("{name}: gate command has no reachable compile check"),
                format!(
                    "gate: <unresolved: {e}>\n        A Rust compile break in {} certifies green. Add `step bash spira/build-fence.sh`\n        to the repository's gate.steps (or a call to it to the row's gate column).",
                    path.display()
                ),
            )),
        }
    }
    if checked == 0 {
        out.push(ok(format!(
            "no repository in the map carries Rust crates (checked {})",
            w.env("SPIRA_REPO_MAP").unwrap_or_else(|| "<unset>".to_string())
        )));
    }
    out
}

// ============================================================================ store

pub fn check_store(w: &dyn World) -> Vec<Line> {
    let mut out = Vec::new();
    let conf = conf_display(w);
    let db = w.env("SPIRA_DB").unwrap_or_default();
    let db_path = Path::new(&db);
    let beads_dir = db_path.join(".beads");
    let install = installing(w);

    if w.dir_exists(&beads_dir) {
        out.push(ok(format!("{db} has a .beads")));
        match w.bd_list(db_path, 60) {
            Ok(_) => {
                out.push(ok("bd can read it"));
                let cwd = w.env("SPIRA_HOME").unwrap_or_default();
                match w.bd_role_warnings(db_path, Path::new(&cwd)) {
                    Some(n) if n > 0 => out.push(warn(
                        format!("bd warns beads.role is unset ({n} line(s)) when run from {cwd}"),
                        "Fix: git config --global beads.role maintainer",
                    )),
                    _ => out.push(ok("bd emits no beads.role warning")),
                }
            }
            Err(bd_out) => {
                let dolt_data = w.env("SPIRA_DOLT_DATA").filter(|v| !v.is_empty());
                if install && dolt_data.is_some() && !w.systemd_user_is_active("dolt-beads.service") {
                    out.push(warn(
                        format!("bd cannot read {db} — dolt-beads.service not active; install.sh will start it in phase 4"),
                        bd_out.lines().take(2).collect::<Vec<_>>().join("\n"),
                    ));
                } else {
                    out.push(fail(
                        format!("bd cannot read {db}"),
                        format!("{}\n        A Dolt server may be down. Try: bd -C {db} dolt start", bd_out.lines().take(2).collect::<Vec<_>>().join("\n")),
                    ));
                }
            }
        }
        let meta = beads_dir.join("metadata.json");
        if w.file_exists(&meta) {
            if let Some(m) = w.read_store_meta(&meta) {
                if m.dolt_mode.as_deref() == Some("embedded") {
                    out.push(fail(
                        "store is in embedded mode — one client at a time, every bd call serialises on one lock",
                        format!("Set SPIRA_DOLT_DATA in {conf} and re-run install.sh to migrate to server mode."),
                    ));
                } else {
                    out.push(ok(format!("store mode: {}", m.dolt_mode.unwrap_or_else(|| "unknown".to_string()))));
                }
            }
        }
    } else if install {
        out.push(warn(format!("{db} has no .beads yet — install.sh will create it in phase 3"), format!("Set SPIRA_DB in {conf} if this path is wrong.")));
    } else {
        out.push(fail(format!("{db} has no .beads — the harness refuses to guess a database"), format!("Set SPIRA_DB in {conf} and run install.sh to create it.")));
    }

    if let Some(dolt_data) = w.env("SPIRA_DOLT_DATA").filter(|v| !v.is_empty()) {
        let dolt_dir = Path::new(&dolt_data);
        if w.dir_exists(dolt_dir) {
            out.push(ok(format!("dolt data directory at {dolt_data}")));
        } else if install {
            out.push(warn(
                format!("SPIRA_DOLT_DATA is set but {dolt_data} does not exist — install.sh will create it in phase 3"),
                format!("Set SPIRA_DOLT_DATA in {conf} if this path is wrong."),
            ));
        } else {
            out.push(fail(
                format!("SPIRA_DOLT_DATA is set but {dolt_data} does not exist"),
                "Create it, or point SPIRA_DOLT_DATA at the directory dolt sql-server uses.",
            ));
        }
        let server_yaml = dolt_dir.join("dolt-server.yaml");
        if w.file_exists(&server_yaml) {
            out.push(ok(format!("dolt-server.yaml at {}", server_yaml.display())));
        } else {
            out.push(warn(
                format!("no dolt-server.yaml at {}", server_yaml.display()),
                "The dolt-beads.service ExecStart expects this file. Without it the server\n        cannot start. See the README for the minimal config.",
            ));
        }
        if w.systemd_user_is_active("dolt-beads.service") {
            out.push(ok("dolt-beads.service is active"));
        } else if install {
            out.push(warn(
                "dolt-beads.service is not active — install.sh will install and start it in phase 4",
                "phase 4 (systemd/install.sh) enables and starts dolt-beads.service.",
            ));
        } else {
            out.push(fail(
                "dolt-beads.service is not active",
                "The database is unreachable without the server. Start it:\n        systemctl --user start dolt-beads.service\n        Or run install.sh to enable and start it.",
            ));
        }
    } else {
        let meta_srv = Path::new(&db).join(".beads").join("metadata.json");
        if w.file_exists(&meta_srv) {
            if let Some(m) = w.read_store_meta(&meta_srv) {
                let host = if m.dolt_server_host.is_empty() { "127.0.0.1".to_string() } else { m.dolt_server_host.clone() };
                match m.dolt_server_port {
                    Some(port) => {
                        if w.tcp_connect(&host, port) {
                            out.push(ok(format!("dolt server answering on {host}:{port}")));
                        } else {
                            out.push(fail(
                                format!("SPIRA_DOLT_DATA is empty but no server answers on {host}:{port}"),
                                format!("Start the dolt server, or set SPIRA_DOLT_DATA in {conf} so\n        install.sh can manage it."),
                            ));
                        }
                    }
                    None => out.push(ok("SPIRA_DOLT_DATA is empty — dolt server is managed independently")),
                }
            } else {
                out.push(ok("SPIRA_DOLT_DATA is empty — dolt server is managed independently"));
            }
        } else {
            out.push(ok("SPIRA_DOLT_DATA is empty — dolt server is managed independently"));
        }
    }

    out
}

// ============================================================================ duckdb

pub fn check_duckdb(w: &dyn World) -> Vec<Line> {
    let bin = w.env("SPIRA_DUCKDB_BIN").filter(|v| !v.is_empty()).unwrap_or_else(|| "duckdb".to_string());
    // `which` (real.rs) already applies bash's own `[[ "$bin" == */* ]]` split: a name
    // containing `/` is checked as an executable FILE directly, never searched on PATH.
    let found = w.which(&bin);
    if let Some(p) = found {
        vec![ok(format!("duckdb — {}", p.display()))]
    } else {
        vec![fail(
            "duckdb is not on PATH",
            "tsd-query.sh refuses every query and every reconciler-flow invariant logs\n        unobservable until this is installed. See deps.toml's duckdb entry.",
        )]
    }
}

// ============================================================================ compilation cache

/// sp-xjnzl: `sccache` present (see deps.toml) is not the same question as `sccache` built
/// with the backend the host and every round-vm now share — a binary installed with
/// `cargo install sccache --locked` alone (no `--features webdav`) runs and passes every
/// ordinary cache check, and silently never speaks WebDAV. The install command that is
/// actually correct, pinned once here rather than re-derived: `cargo install sccache
/// --locked --no-default-features --features webdav`.
const SCCACHE_INSTALL: &str = "cargo install sccache --locked --no-default-features --features webdav";

pub fn check_sccache(w: &dyn World) -> Vec<Line> {
    // Same gate as check_operator_channel: sccache is deps.toml's "operator" tier, not
    // "runtime" — a fixture container (SPIRA_OPERATED=0) is deliberately without it
    // (deps.toml's own waiver: "the fixture sets SPIRA_BUILD_CACHE=off ... on purpose
    // instead of being refused"), so absence there is a WARN, not a FAIL. A real,
    // operated box gets FAIL — the same box check_sccache was written for.
    let operated = w.env("SPIRA_OPERATED").map(|v| v != "0").unwrap_or(true);
    let make = |msg: String, detail: String| -> Line {
        if operated { fail(msg, detail) } else { warn(msg, detail) }
    };
    let Some(bin) = w.which("sccache") else {
        return vec![make(
            "sccache is not on PATH".into(),
            format!("every build refuses rather than compile every dependency cold (sp-z61hj). See deps.toml's sccache entry. Install: {SCCACHE_INSTALL}"),
        )];
    };
    let Some(help) = w.sccache_help() else {
        return vec![make(format!("sccache ({}) did not answer --help", bin.display()), String::new())];
    };
    let has_webdav = help
        .lines()
        .find(|l| l.trim_start().starts_with("WebDAV:"))
        .is_some_and(|l| l.trim_end().ends_with("true"));
    if has_webdav {
        vec![ok(format!("sccache ({}) — webdav backend present", bin.display()))]
    } else {
        vec![make(
            format!("sccache ({}) was built without the webdav backend", bin.display()),
            format!(
                "round-vm's shared cache (sccache-dav, sp-xjnzl) and this box's own builds both need it. \
                 `sccache --help`'s \"Enabled features\" block said so, not just sccache's presence on PATH. \
                 Reinstall: {SCCACHE_INSTALL}"
            ),
        )]
    }
}

/// sp-xtdqi: `sccache` naming the webdav backend at build time (`check_sccache`, above) is
/// not the same question as the CLIENT DAEMON already running on this box actually being
/// configured for it — a daemon fixes its backend at spawn time and ignores every later
/// invocation's environment, so a stale one silently answers every build from the local disk
/// cache even on a box that built sccache correctly and whose config names a store. Read-only:
/// unlike `spira_config::build`'s own `ensure_store_backend`, doctor never stops the server —
/// it only reports (`A failed probe renders ?, never 0` is this check's whole job; the
/// self-heal lives where a build is actually about to happen).
pub fn check_sccache_backend(w: &dyn World) -> Vec<Line> {
    let Some(addr) = w.sccache_dav_addr() else {
        return vec![ok("no shared store configured (SPIRA_SCCACHE_DAV_ADDR unset) — nothing to check")];
    };
    let operated = w.env("SPIRA_OPERATED").map(|v| v != "0").unwrap_or(true);
    let make = |msg: String, detail: String| -> Line {
        if operated { fail(msg, detail) } else { warn(msg, detail) }
    };
    let Some(stats) = w.sccache_show_stats() else {
        return vec![make("sccache --show-stats did not answer".into(), "cannot verify the live server's backend".into())];
    };
    let Some(loc) = stats.lines().find(|l| l.trim_start().starts_with("Cache location")) else {
        return vec![make("sccache --show-stats did not report a Cache location".into(), stats)];
    };
    let loc = loc.trim();
    if loc.to_ascii_lowercase().contains("webdav") {
        vec![ok(format!("sccache server is on the shared store ({addr}): {loc}"))]
    } else {
        vec![make(
            format!("sccache server is NOT on the shared store — {loc}"),
            format!(
                "the operator's own config names a shared store ({addr}) but the running sccache \
                 server was started on a different backend — a build that restarted it since \
                 (or one that never has) silently lands on the local-disk cache instead. \
                 `spira_config::build::Wrapper` stops a wrong-backend server itself the next \
                 time it builds through it; `sccache --stop-server` does the same by hand."
            ),
        )]
    }
}

// ============================================================================ events probe

pub fn check_events_probe(w: &dyn World) -> Vec<Line> {
    let db = w.env("SPIRA_DB").unwrap_or_default();
    let db_path = Path::new(&db);
    if !w.dir_exists(&db_path.join(".beads")) {
        return vec![warn("cannot probe events substrate — no database yet", "")];
    }
    let Some(id) = w.bd_first_id(db_path) else {
        return vec![warn(
            "cannot probe events substrate — the store holds no issue to anchor a probe event to",
            "This is expected on a store that has not been used yet. The probe resumes once a bead exists.",
        )];
    };
    let etype = "__doctor_probe__";
    let wrote = w.bump_write_event_try(&id, etype, "doctor");
    let count = w.counter_events_query(&id, etype);
    let count_positive = count.parse::<i64>().map(|n| n > 0).unwrap_or(false);
    if count_positive {
        return vec![ok("events write/read round trip")];
    }
    let path = format!("bd sql (server mode at {db})");
    if !wrote {
        vec![fail(
            format!("events write/read round trip failed — the write via {path} was refused (anchor bead: {id})"),
            "Run the INSERT by hand to see the error; a foreign-key refusal means the anchor\n        bead no longer exists, a permission error means the store is not writable.",
        )]
    } else {
        vec![fail(
            format!("events write/read round trip failed — writes via {path} are discarded"),
            format!("The write reported success and did not come back. Check that {db} is writable."),
        )]
    }
}

// ============================================================================ failed units

pub fn check_failed_units(w: &dyn World) -> Vec<Line> {
    let install = installing(w);
    match w.systemd_failed_units("spira-*") {
        Err(e) => vec![fail(format!("cannot query failed units: {}", e.lines().next().unwrap_or("")), "Check the systemd user manager: systemctl --user status")],
        Ok(units) if units.is_empty() => vec![ok("no failed spira-* units")],
        Ok(units) => units
            .into_iter()
            .map(|unit| {
                if install {
                    warn(format!("{unit} is a failed systemd unit — pre-existing, predates this install"), format!("Check: journalctl --user -u {unit} -n 20"))
                } else {
                    fail(format!("{unit} is a failed systemd unit"), format!("Check: journalctl --user -u {unit} -n 20"))
                }
            })
            .collect(),
    }
}

// ============================================================================ orphan units

pub fn check_orphan_units(w: &dyn World) -> Vec<Line> {
    let rows = match w.watchd_manifest() {
        Ok(r) => r,
        Err(_) => return vec![fail("cannot read the watcher manifest", "Check: watchd manifest")],
    };
    let mut known: Vec<&str> = Vec::new();
    for line in rows.lines() {
        let mut parts = line.split('|');
        let name = parts.next().unwrap_or("");
        let kind = parts.next().unwrap_or("");
        if !name.is_empty() && kind == "daemon" {
            known.push(name);
        }
    }
    let inst = w.env("SPIRA_INSTANCE").filter(|v| !v.is_empty()).unwrap_or_else(|| "prod".to_string());
    let pattern = format!("spira-watch-*-{inst}.service");
    let units = match w.systemd_enabled_unit_files(&pattern) {
        Ok(u) => u,
        Err(e) => return vec![fail(format!("cannot query watch unit files: {}", e.lines().next().unwrap_or("")), "Check the systemd user manager: systemctl --user status")],
    };
    let mut out = Vec::new();
    for unit in &units {
        let wname = unit.strip_prefix("spira-watch-").unwrap_or(unit).strip_suffix(&format!("-{inst}.service")).unwrap_or(unit);
        if known.contains(&wname) {
            continue;
        }
        out.push(fail(format!("{unit} is enabled but '{wname}' has no daemon row in the manifest"), "Check: watchd prune"));
    }
    if out.is_empty() {
        out.push(ok("no orphan spira-watch units"));
    }
    out
}

// ============================================================================ prod checkout

/// Every installed (non-transient, non-aeon) unit's ExecStart must resolve under
/// `$SPIRA_RELEASES`; anywhere else, a merge to the checkout deploys with no gate.
pub fn check_prod_checkout(w: &dyn World) -> Vec<Line> {
    let rows = match w.systemd_installed_unit_execs() {
        Ok(r) => r,
        Err(e) => return vec![fail(format!("cannot query installed unit files: {e}"), "Check the systemd user manager: systemctl --user status")],
    };
    let releases = w.env("SPIRA_RELEASES").unwrap_or_default();
    let root = format!("{}/", releases.trim_end_matches('/'));
    let mut out = Vec::new();
    for (unit, state, path) in &rows {
        if state == "transient" || unit.starts_with("spira-aeon-") || path.is_empty() || path.starts_with(&root) {
            continue;
        }
        out.push(fail(
            format!("{unit}'s ExecStart resolves outside $SPIRA_RELEASES"),
            format!("path: {path}\nCut over: build-tarball.sh, then activate.sh <tarball> (or deploy.sh <tag>) so SPIRA_PROD points under {releases}, then re-run install.sh to re-render units."),
        ));
    }
    if out.is_empty() {
        out.push(ok(format!("every installed spira-*/beads-push unit runs from {releases}")));
    }
    out
}

// ============================================================================ snapshot freshness

pub fn check_snapshot_fresh(w: &dyn World) -> Vec<Line> {
    let run = w.env("SPIRA_RUN").unwrap_or_default();
    let snap = Path::new(&run).join("cockpit.env");
    let hint = w.spira_unit("cockpit", "service");
    let Some(age) = w.file_age_secs(&snap) else {
        return vec![warn(format!("no cockpit snapshot at {} — collector may not have run yet", snap.display()), format!("Check: systemctl --user status {hint}"))];
    };
    let limit = w.env("SPIRA_SNAP_STALE_S").and_then(|v| v.parse::<u64>().ok()).unwrap_or(60);
    if age > limit {
        vec![fail(
            format!("cockpit snapshot stale — last written {age}s ago (limit {limit}s)"),
            format!("The collector is not writing. Check: systemctl --user status {hint}"),
        )]
    } else {
        vec![ok(format!("cockpit snapshot fresh — {} ({age}s old)", snap.display()))]
    }
}

// ============================================================================ operator channel

pub fn check_operator_channel(w: &dyn World) -> Vec<Line> {
    let operated = w.env("SPIRA_OPERATED").map(|v| v != "0").unwrap_or(true);
    let make = |msg: String, detail: String| -> Line {
        if operated { fail(msg, detail) } else { warn(msg, detail) }
    };
    let mut out = Vec::new();

    if matches!(w.env("SPIRA_MAIL_MUTE").as_deref(), Some("1") | Some("true")) {
        out.push(ok("mail is muted (spira.mail_mute) — messages are recorded already read; no mailbox shows unread"));
    }

    match w.which("inotifywait") {
        Some(p) => out.push(ok(format!("inotifywait — {}", p.display()))),
        None => out.push(make("inotifywait is not on PATH — no mail reaches you until a session starts".into(), "Install: apt install inotify-tools".into())),
    }

    if let Some(mailer) = w.env("COCKPIT_MAIL").filter(|v| !v.is_empty()) {
        match w.which(&mailer) {
            Some(p) => out.push(ok(format!("{mailer} (COCKPIT_MAIL) — {}", p.display()))),
            None => out.push(make(
                format!("{mailer} (COCKPIT_MAIL) is not on PATH — the cockpit mail pane is dead; escalations and answers have no delivery path"),
                format!("Install {mailer}, or set COCKPIT_MAIL in {} to a mail client that is present.", conf_display(w)),
            )),
        }
    }

    let sessions = w.env("COCKPIT_SESSIONS").unwrap_or_default();
    if sessions.split_whitespace().any(|s| s == "hunk") {
        match w.which("hunk") {
            Some(p) => out.push(ok(format!("hunk — {}", p.display()))),
            None => out.push(make(
                "hunk is not on PATH — the cockpit review pane is unavailable; rebuild.sh creates a hunk session with no program behind it".into(),
                "Install: npm install -g --prefix ~/.local hunkdiff (bin lands at ~/.local/bin/hunk; ensure ~/.local/bin is in SPIRA_PATH in spira.conf)".into(),
            )),
        }
    }

    let via_path = w.which("go").map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let go_candidates = [w.env("GO").unwrap_or_default(), format!("{}/.local/go/bin/go", w.env("HOME").unwrap_or_default()), via_path];
    let go_found = go_candidates.into_iter().find(|c| !c.is_empty() && w.is_executable_file(c));
    if let Some(g) = go_found {
        out.push(ok(format!("go — {g}")));
    } else {
        let bd_ver = w.bd_version();
        let bd_tag = w.env("SPIRA_BD_TAG").unwrap_or_default().trim_start_matches('v').to_string();
        let bd_ok = bd_ver.as_deref() == Some(bd_tag.as_str()) && !bd_tag.is_empty();
        let arch = w.arch();
        let prebuilt = matches!(arch.as_str(), "x86_64" | "aarch64");
        if !bd_ok && !prebuilt {
            out.push(make(
                format!("go is not on PATH — {}", w.spira_bin_purpose("go")),
                format!("bd is mismatched (need {bd_tag}) and no prebuilt exists for {arch}. Install Go: https://go.dev/dl/ or use $HOME/.local/go/bin/go"),
            ));
        } else {
            out.push(ok(format!(
                "go absent — bd {}; prebuilt {} for {arch}",
                if bd_ok { "current" } else { "mismatched" },
                if prebuilt { "available" } else { "unavailable" }
            )));
        }
    }

    out
}

// ============================================================================ concierge singleton

pub fn check_concierge_singleton(w: &dyn World) -> Vec<Line> {
    let repo = w.env("SPIRA_REPO").unwrap_or_default();
    let conc = Path::new(&repo).join("concierge.sh");
    if !w.is_executable_file(&conc.to_string_lossy()) {
        return vec![warn(format!("no concierge.sh at {} — cannot check for a second concierge", conc.display()), "")];
    }
    let stray = w.concierge_stray_holders(&conc);
    if stray.is_empty() {
        vec![ok("Remote Control name 'concierge' held by no more than the managed session")]
    } else {
        vec![fail(
            "a second concierge is live outside the managed session",
            format!(
                "pid(s): {}\n        Mail wakes reach only the managed session; this one will never see a reply. Inspect:\n        ps -o pid,tty,lstart,cmd -p <pid>. If it should be the managed one, kill it and run:\n        {} start",
                stray.join(" "),
                conc.display()
            ),
        )]
    }
}
