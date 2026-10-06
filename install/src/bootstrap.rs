//! Shared setup for the `units-install` and `unit-ensure` binaries: resolving host values and
//! the manifest from the environment a caller (`deploy.sh`, `landing-pass`, a human) already
//! set, exactly as `systemd/install.sh` and `unit-ensure.sh` both did by sourcing the same
//! `conf.sh`/`units.sh`. Kept out of the library's own tested modules (which take everything
//! as plain values) so the environment-reading glue is the only thing duplicated nowhere.

use crate::manifest::{self, Manifest};
use crate::values::HostValues;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn env_var(k: &str) -> String {
    env::var(k).unwrap_or_default()
}

pub fn nonempty_env(k: &str) -> Option<String> {
    env::var(k).ok().filter(|v| !v.is_empty())
}

pub fn which(prog: &str) -> Option<String> {
    let path = env::var("PATH").ok()?;
    for dir in path.split(':') {
        let p = Path::new(dir).join(prog);
        if p.is_file() {
            return Some(p.to_string_lossy().to_string());
        }
    }
    None
}

/// Resolve host values from the environment (conf.sh's own precedence: explicit environment
/// wins). Derives nothing beyond what conf.sh itself derives with a plain, no-side-effect
/// default for a render-relevant key: `SPIRA_HOME = SPIRA_REPO/spira` (conf.sh: no-colon
/// derivation), `SPIRA_COCKPIT = dirname(SPIRA_HOME)/cockpit` (conf.sh line ~937),
/// `SPIRA_SNAP_STALE_S = 60` (line ~951), `SPIRA_TESTDB_PORT = 3308` (line ~1015) — the three
/// `: "${VAR:=default}"` conf.sh lines that matter to rendering. A caller that already
/// sourced conf.sh (a human, `deploy.sh`) exports the real value first, so this default is
/// reached only when nothing did — never a silent override of an explicit setting.
///
/// NEITHER SET (sp-al35q): `SPIRA_REPO/spira` with an empty `SPIRA_REPO` rendered the
/// literal string "/spira" into every unit's substituted content — not a missing file (that
/// would at least be loud), a wrong one. test-unit-drift.sh's "matching units" case, which
/// deliberately sets neither (the same real-world shape as pre-activate, sp-w1r4f), rendered
/// every single unit with this bogus home baked in and reported DIFFERS across the board.
/// Falls back to the same release-relative resolution `templates_dir()` uses — `<release>/
/// spira` beside the binary's own `bin/`, verified by requiring `conf.sh` there, exactly as
/// `templates_dir`'s own fallback requires `spira-sentinel.service` under its `systemd/`
/// candidate — and refuses outright when even that fails, rather than ever rendering
/// "/spira" again.
///
/// `repo` falls back to `derive_repo_filesystem` when unset: units substitute it into
/// `ExecStart=`, and an empty value bakes `--repo ` into the installed unit.
/// The repository units bake into `--repo` when `SPIRA_REPO` is unset: the repo map's
/// home-repo root when that is a git checkout, else the filesystem derivation. On a release
/// the derivation is the release directory, which has no `.git`, so both cert-sweep units
/// exited 2 on every run ("not a git repository") and deploy's pre-health refused on them
/// (sp-7i16g); the map already names the real checkout.
pub fn units_repo(mapped: Option<String>, derived: String, is_git: impl Fn(&Path) -> bool) -> String {
    match mapped.filter(|m| !m.is_empty() && is_git(Path::new(m))) {
        Some(m) => m,
        None => derived,
    }
}

/// The mailbox names `SPIRA_MAIL_READERS` registers (`name=command` entries, whitespace-
/// separated) — a registered key (spira/conf.d), through the one door, never a
/// competing environment override (per Ryan 2026-10-05: one source of config).
pub fn reader_mailboxes() -> Result<Vec<String>, String> {
    let raw = spira_config::process::cfg("SPIRA_MAIL_READERS")?;
    Ok(parse_reader_mailboxes(&raw))
}

fn parse_reader_mailboxes(raw: &str) -> Vec<String> {
    raw.split_whitespace().filter_map(|e| e.split('=').next()).map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).collect()
}

/// `mail ensure <name>` for every registered reader mailbox (sp-xp0u2). A fresh install made
/// them; an upgrade's re-render (units-install alone) never did, so `mail-health` found
/// `concierge: no such mailbox` after every upgrade and spira-notify failed 3.
pub fn ensure_reader_mailboxes() -> Result<(), String> {
    for name in reader_mailboxes()? {
        let ok = Command::new("timeout").args(["5", "mail", "ensure", &name]).status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            return Err(format!("could not create the {name} mailbox (mail ensure {name})"));
        }
    }
    Ok(())
}

/// Every registered key below goes through `spira_config::process::cfg`/`cfg_parse` — the
/// declared value in `$SPIRA_TOML`, never a competing environment override (per Ryan
/// 2026-10-05: one source of config). The three historical incidents this module used to
/// cite for "the environment wins" (sp-xp0u2, sp-al35q, sp-7i16g — a predecessor's trimmed
/// deploy environment needing to override a fresh, possibly-wrong resolution) are handled
/// upstream now: the predecessor sets `$SPIRA_TOML` to the right file, rather than this
/// binary trusting an exported override on top of whatever `$SPIRA_TOML` says.
pub fn host_from_env(instance: &str) -> Result<HostValues, String> {
    let repo_env = nonempty_env("SPIRA_REPO");
    let home = resolve_home(nonempty_env("SPIRA_HOME"), repo_env.clone(), argv0_path().as_deref())?;
    let repo = repo_env.unwrap_or_else(|| {
        let env_map: std::collections::BTreeMap<String, String> = env::vars().collect();
        let mapped = spira_config::repos::Registry::from_env(env_map.clone(), Path::new(&home)).root("");
        let derived = spira_config::resolve::derive_repo_filesystem(Path::new(&home), &env_map).to_string_lossy().into_owned();
        units_repo(mapped, derived, |p| p.join(".git").exists())
    });
    let cockpit = spira_config::process::cfg("SPIRA_COCKPIT")?;
    let dav_addr = sccache_dav_addr()?;
    let dolt = nonempty_env("DOLT").or_else(|| which("dolt")).unwrap_or_default();
    // SPIRA_LC_PASSWORD_FILE is PROCEDURAL (spira/conf.d): its default depends on whether
    // the same-user credential file exists RIGHT NOW, which a value baked into `$SPIRA_TOML`
    // at generation time could not track — `spira_config::resolve::lc_credential_default`
    // (not `cfg`) is this key's own correct door, same as before this bead, minus the
    // environment-override rung.
    let env_map: std::collections::BTreeMap<String, String> = env::vars().collect();
    let lc_password_file = spira_config::resolve::lc_credential_default(&env_map);
    // `resolve_run_dir` (not bare `cfg("SPIRA_RUN")`) because it also runs the
    // instance-containment check `cfg()` does not; its own doc now says "no environment
    // override" too, so no override rung remains here either.
    let run = spira_config::resolve::resolve_run_dir(&env_map, Path::new(&home)).map(|p| p.to_string_lossy().into_owned())?;
    if run.is_empty() {
        return Err("SPIRA_RUN is unset and spira.run resolved empty — refusing to render units that would log to the filesystem root".to_string());
    }
    let db = spira_config::process::cfg("SPIRA_DB")?;
    let repo_map = spira_config::process::cfg("SPIRA_REPO_MAP")?;
    let dolt_data = spira_config::process::cfg("SPIRA_DOLT_DATA")?;
    let testdb_data = spira_config::process::cfg("SPIRA_TESTDB_DATA")?;
    Ok(HostValues {
        home,
        lc_password_file,
        repo,
        run,
        db,
        repo_map,
        cockpit,
        dolt_data,
        testdb_data,
        dolt,
        prod: spira_config::process::cfg("SPIRA_PROD")?,
        instance: instance.to_string(),
        testdb_port: spira_config::process::cfg("SPIRA_TESTDB_PORT")?,
        snap_stale_s: spira_config::process::cfg("SPIRA_SNAP_STALE_S")?,
        watchtower_start_timeout_s: spira_config::process::cfg("SPIRA_WATCHTOWER_START_TIMEOUT_S")?,
        path_tail: crate::orchestrate::path_tail().unwrap_or_default(),
        // sp-xtdqi-2: the key name lives once, in `release::units` — `release`'s own
        // activate/render gate reads the same constant, never a second hand-written literal.
        sccache_dav_addr: dav_addr,
    })
}

/// `SPIRA_SCCACHE_DAV_ADDR`, a registered key — through the one door, never the bare
/// environment alone (law-a-binary-resolves-the-config-it-reads).
pub fn sccache_dav_addr() -> Result<String, String> {
    spira_config::process::cfg(release::units::SCCACHE_DAV_ADDR_KEY)
}

/// `SPIRA_HOME`, else `SPIRA_REPO/spira`, else the nearest `spira/` holding `conf.sh` above
/// this binary (a release's `bin/../spira`), else refuse — never the bare, unverified
/// `"{repo}/spira"` string sp-al35q let through when `repo` was itself empty. Pure (takes
/// `exe` explicitly) so the no-env refusal and the release-relative fallback are both
/// testable without touching the real environment or `argv0_path()`'s real `argv[0]`.
fn resolve_home(home_env: Option<String>, repo_env: Option<String>, exe: Option<&Path>) -> Result<String, String> {
    if let Some(h) = home_env {
        return Ok(h);
    }
    if let Some(r) = repo_env {
        return Ok(format!("{r}/spira"));
    }
    match exe.and_then(spira_above) {
        Some(p) => Ok(p.to_string_lossy().into_owned()),
        None => Err(
            "cannot resolve SPIRA_HOME: neither SPIRA_HOME nor SPIRA_REPO is set, and no spira/ \
             (holding conf.sh) was found beside this binary's own release"
                .to_string(),
        ),
    }
}

fn spira_above(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().skip(1).map(|a| a.join("spira")).find(|d| d.join("conf.sh").is_file())
}

/// `watchd units` — the watcher manifest. `watchd.sh` is retired (sp-48f6g: rewritten to
/// the `watchd` crate); called by bare name exactly as units.sh did, just the new binary
/// name. `watchd` shells into `conf.sh` itself for everything conf.sh would otherwise
/// derive (`watchd::context`'s one seam), so `SPIRA_WATCHERS`'s own conf.sh default
/// (`$SPIRA_HOME/watchers`) reaches it for free as long as `SPIRA_HOME` is set — this
/// fallback stays as a second line of defense for a minimal env that sets neither.
pub fn watch_names() -> Result<Vec<String>, String> {
    let watchers = nonempty_env("SPIRA_WATCHERS").or_else(|| nonempty_env("SPIRA_HOME").map(|h| format!("{h}/watchers")));
    let mut cmd = Command::new("watchd");
    cmd.arg("units");
    if let Some(w) = watchers {
        cmd.env("SPIRA_WATCHERS", w);
    }
    let out = cmd.output().map_err(|e| format!("cannot run watchd: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        return Err(format!("the watcher manifest is malformed: {}", why.trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.split_whitespace().map(|u| u.strip_prefix("spira-watch@").unwrap_or(u).strip_suffix(".service").unwrap_or(u).to_string()).collect())
}

/// `ctrl suspended`'s TSV (`subject<TAB>reason` per line, sp-6onps) — the control plane
/// moved from a bash library (`CTRL_LIB=1 . ctrl.sh`) to a compiled binary, which cannot be
/// sourced, so install.sh now reads every suspension once this way instead of parsing
/// `ctrl.sh list --json`'s render. A missing/failing `ctrl` means nothing is suspended,
/// matching the bash fallback.
pub fn suspended_set() -> std::collections::BTreeSet<String> {
    let out = Command::new("ctrl").arg("suspended").output();
    let Ok(out) = out else { return Default::default() };
    if !out.status.success() {
        return Default::default();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().filter_map(|l| l.split('\t').next()).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

pub fn unit_dir() -> PathBuf {
    if let Some(d) = nonempty_env("SPIRA_UNIT_DIR") {
        return PathBuf::from(d);
    }
    crate::orchestrate::default_unit_dir(&nonempty_env).unwrap_or_else(|| PathBuf::from(".config/systemd/user"))
}

pub fn templates_dir() -> PathBuf {
    let exe = argv0_path();
    templates_dir_from(nonempty_env("SPIRA_HOME"), nonempty_env("SPIRA_REPO"), exe.as_deref())
}

/// `SPIRA_HOME/../systemd`, else `SPIRA_REPO/systemd`, else the nearest `systemd/` holding unit
/// templates above this binary (a release's `bin/../systemd`), else a cwd-relative `systemd`.
/// The binary's own location is what the retired systemd/install.sh used (its own dirname);
/// without it, pre-activate — which sets neither variable — looked in its cwd and every
/// release from sp-31dm0 on failed verify (sp-w1r4f).
pub fn templates_dir_from(home: Option<String>, repo: Option<String>, exe: Option<&Path>) -> PathBuf {
    let d = home
        .map(|h| PathBuf::from(h).join("../systemd"))
        .or_else(|| repo.map(|r| PathBuf::from(r).join("systemd")))
        .or_else(|| exe.and_then(systemd_above))
        .unwrap_or_else(|| PathBuf::from("systemd"));
    d.canonicalize().unwrap_or(d)
}

fn systemd_above(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().skip(1).map(|a| a.join("systemd")).find(|d| d.join("spira-sentinel.service").is_file())
}

/// `argv[0]`, resolved to where it actually sits, but with every symlink component left
/// exactly as invoked — never `env::current_exe()`'s fully resolved path.
///
/// `current_exe()` canonicalizes every symlink in the path; testenv's own release staging
/// (and the gate's fixture release) link `bin/<tool>` to wherever cargo actually built it
/// and `systemd/`/`spira/` to the real checkout — two unrelated directories once resolved,
/// so `systemd_above`'s walk from a `current_exe()` path landed in cargo's own target/
/// directory, which has no `systemd/` sibling at all, and `templates_dir()` fell through to
/// a cwd-relative guess (caught live by testenv's test-unit-drift.sh: every unit showed
/// DIFFERS because the "clean" comparison rendered from the wrong templates, sp-yyk47).
///
/// A bare name (no `/`) is NOT already the resolved path the way it is when a shell execs a
/// PATH-found command: bash rewrites its own `$0` to the full path PATH search landed on
/// before exec, but a non-shell caller — `env PATH=... units-install --diff`, a direct
/// `execvp`/`posix_spawnp` — calls `execvp` directly, which resolves the PATH search
/// internally but passes argv[0] through to the new process completely unchanged.
/// A refusal when `exe` lives in a release that is not the one `current` names: such a
/// binary would re-render every installed unit back to its own, older release.
pub fn stale_release_refusal(tool: &str, exe: &Path) -> Option<String> {
    let skew = spira_config::release_skew::skew(exe.parent()?.parent()?)?;
    Some(format!(
        "{tool}: REFUSING to write units — {}; an old release never downgrades the installed units. Run it from {}/bin",
        skew.describe(),
        skew.current.display()
    ))
}

pub fn refuse_if_stale_release(tool: &str) -> Option<String> {
    stale_release_refusal(tool, &std::env::current_exe().ok()?)
}

fn argv0_path() -> Option<PathBuf> {
    let arg0 = env::args_os().next()?;
    let p = PathBuf::from(&arg0);
    if p.components().count() > 1 {
        return if p.is_absolute() { Some(p) } else { Some(env::current_dir().ok()?.join(p)) };
    }
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(&arg0);
        if is_exec(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_exec(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// Build this box's manifest from the environment, printing units.sh's own informational
/// notes to stderr.
pub fn manifest_from_env(instance: &str) -> Result<Manifest, String> {
    let inotify_present = which("inotifywait").is_some();
    // Same resolution host_from_env gives `--repo` (the repo map's home-repo root when it is
    // a git checkout, else the release dir): a failure to resolve it at all means the rest of
    // the install fails too, so treating it as "not a checkout" here costs nothing extra.
    let repo_is_git_checkout = host_from_env(instance).map(|h| Path::new(&h.repo).join(".git").exists()).unwrap_or(false);
    let m = manifest::build(&manifest::Inputs {
        instance: instance.to_string(),
        dolt_data_set: nonempty_env("SPIRA_DOLT_DATA").is_some(),
        testdb_data_set: nonempty_env("SPIRA_TESTDB_DATA").is_some(),
        // SPIRA_BROKER_ENABLE is a registered key — the one door, never a bare env read.
        broker_enable: spira_config::process::cfg("SPIRA_BROKER_ENABLE")? == "1",
        inotify_present,
        sccache_dav_addr_set: !sccache_dav_addr()?.is_empty(),
        repo_is_git_checkout,
        lc_system_mode: spira_config::resolve::lc_system_mode(),
        watch_names: watch_names(),
    })?;
    for note in &m.notes {
        eprintln!("note: {note}");
    }
    Ok(m)
}

pub fn world_halted() -> bool {
    nonempty_env("SPIRA_RUN").map(|r| Path::new(&r).join("world.halted").exists()).unwrap_or(false)
}

#[cfg(test)]
mod templates_dir_tests {
    use super::*;

    #[test]
    fn with_no_env_the_templates_are_found_beside_the_binarys_release() {
        let t = testkit::TempDir::new("tpl-dir");
        let rel = t.path().join("rel");
        std::fs::create_dir_all(rel.join("bin")).unwrap();
        std::fs::create_dir_all(rel.join("systemd")).unwrap();
        std::fs::write(rel.join("systemd/spira-sentinel.service"), "[Unit]\n").unwrap();
        let got = templates_dir_from(None, None, Some(&rel.join("bin/units-install")));
        assert_eq!(got, rel.join("systemd").canonicalize().unwrap());
    }

    #[test]
    fn the_environment_still_wins_over_the_binarys_location() {
        let t = testkit::TempDir::new("tpl-dir-env");
        let got = templates_dir_from(None, Some(t.path().display().to_string()), Some(Path::new("/nonexistent/bin/x")));
        assert_eq!(got, t.path().join("systemd"));
    }
}

#[cfg(test)]
mod resolve_home_tests {
    use super::*;

    #[test]
    fn spira_home_wins_over_everything() {
        let got = resolve_home(Some("/explicit/spira".into()), Some("/repo".into()), Some(Path::new("/bin/x")));
        assert_eq!(got, Ok("/explicit/spira".to_string()));
    }

    #[test]
    fn spira_repo_wins_over_the_binarys_location_when_home_is_unset() {
        let got = resolve_home(None, Some("/repo".into()), Some(Path::new("/nonexistent/bin/x")));
        assert_eq!(got, Ok("/repo/spira".to_string()));
    }

    /// NEITHER SET, exe resolves (sp-al35q): derives `<release>/spira` beside the binary's
    /// own location, verified by requiring conf.sh there — the same resolution
    /// `templates_dir()` uses for `systemd/`, applied to `spira/` instead.
    #[test]
    fn with_no_env_home_is_found_beside_the_binarys_release() {
        let t = testkit::TempDir::new("home-dir");
        let rel = t.path().join("rel");
        std::fs::create_dir_all(rel.join("bin")).unwrap();
        std::fs::create_dir_all(rel.join("spira")).unwrap();
        std::fs::write(rel.join("spira/conf.sh"), "# conf.sh\n").unwrap();
        let got = resolve_home(None, None, Some(&rel.join("bin/units-install")));
        assert_eq!(got, Ok(rel.join("spira").to_string_lossy().into_owned()));
    }

    /// NEITHER SET, exe does not resolve to a real spira/ (sp-al35q): refuses outright --
    /// returns Err, never the bare, unverified "/spira" string the bug rendered into every
    /// unit's substituted content before this fix (an empty SPIRA_REPO formatted straight
    /// into "{repo}/spira" with no existence check at all).
    #[test]
    fn with_no_env_and_no_resolvable_exe_it_refuses_rather_than_guessing() {
        let got = resolve_home(None, None, Some(Path::new("/nonexistent/bin/units-install")));
        assert!(got.is_err(), "{got:?}");
    }

    #[test]
    fn with_no_env_and_no_exe_at_all_it_refuses() {
        let got = resolve_home(None, None, None);
        assert!(got.is_err(), "{got:?}");
    }
}

#[cfg(test)]
mod stale_release_tests {
    use super::*;

    #[test]
    fn an_old_release_binary_refuses_to_write_units() {
        let t = testkit::TempDir::new("stale-rel");
        let d = t.path().to_path_buf();
        for r in ["old", "new"] {
            std::fs::create_dir_all(d.join(r).join("bin")).unwrap();
        }
        std::os::unix::fs::symlink("new", d.join("current")).unwrap();
        let msg = stale_release_refusal("unit-ensure", &d.join("old/bin/unit-ensure")).expect("old release must refuse");
        assert!(msg.contains("REFUSING") && msg.contains("/new"), "{msg}");
        assert_eq!(stale_release_refusal("unit-ensure", &d.join("new/bin/unit-ensure")), None);
        assert_eq!(stale_release_refusal("unit-ensure", &d.join("missing/bin/unit-ensure")), None);
    }

    #[test]
    fn units_repo_prefers_a_mapped_git_checkout_over_the_derived_release_dir() {
        let git = |p: &Path| p == Path::new("/h/harness");
        assert_eq!(units_repo(Some("/h/harness".into()), "/r/rel".into(), git), "/h/harness");
        assert_eq!(units_repo(Some("/h/not-git".into()), "/r/rel".into(), git), "/r/rel", "a mapped path that is not a checkout is not used");
        assert_eq!(units_repo(None, "/r/rel".into(), git), "/r/rel", "no map: the derivation, as before");
        assert_eq!(units_repo(Some(String::new()), "/r/rel".into(), git), "/r/rel");
    }
}

#[cfg(test)]
mod host_from_env_tests {
    use super::*;

    /// sp-xp0u2: an upgrade's re-render reaches units-install with SPIRA_HOME pointing into
    /// the release and NO `SPIRA_RUN` in the environment (the predecessor's deploy.sh
    /// re-renders under `env -i <orig env>`). The run dir must still resolve to a real
    /// directory — before the fix it rendered empty, and every unit logged to
    /// `append:/<name>.log`. Per Ryan 2026-10-05 (one source of config), the fix for a
    /// predecessor's trimmed environment is `$SPIRA_TOML` pointing at the right file, not an
    /// environment override on top of it — so this now declares `spira.run` in a real
    /// fixture document instead of leaving config unset and relying on an ambient guess.
    #[test]
    fn run_resolves_from_config_when_the_environment_carries_none() {
        let t = testkit::TempDir::new("host-env-run");
        let home = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("spira");
        let run_dir = t.join("run");
        let toml = spira_config::process::fixture_toml(t.path(), &[("SPIRA_RUN", &run_dir.display().to_string())]);
        let _env = testkit::env(&[
            ("SPIRA_HOME", Some(home.to_str().unwrap())),
            ("SPIRA_RUN", None),
            ("SPIRA_REPO", None),
            ("SPIRA_TOML", toml.to_str()),
            ("SPIRA_CONF", None),
        ]);
        let host = host_from_env("prod").expect("host values resolve");
        assert_eq!(host.run, run_dir.display().to_string(), "the declared spira.run must reach host_from_env");
        assert!(Path::new(&host.run).is_absolute(), "run dir {} is not absolute", host.run);
    }

    /// The companion regression guard: an explicit `SPIRA_RUN` in the environment must NOT
    /// win over the declared config anymore — the opposite of this test's name before this
    /// bead, when "the environment wins" was still host_from_env's own contract.
    #[test]
    fn the_declared_run_wins_over_a_stale_environment_value() {
        let t = testkit::TempDir::new("host-env-run-declared-wins");
        let home = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("spira");
        let run_dir = t.join("declared-run");
        let toml = spira_config::process::fixture_toml(t.path(), &[("SPIRA_RUN", &run_dir.display().to_string())]);
        let _env = testkit::env(&[
            ("SPIRA_HOME", Some(home.to_str().unwrap())),
            ("SPIRA_RUN", Some("/explicit/run")),
            ("SPIRA_REPO", None),
            ("SPIRA_TOML", toml.to_str()),
        ]);
        assert_eq!(host_from_env("prod").expect("host values resolve").run, run_dir.display().to_string());
    }
}

#[cfg(test)]
mod reader_mailbox_tests {
    use super::parse_reader_mailboxes;

    #[test]
    fn names_come_from_each_entry_before_its_command() {
        assert_eq!(parse_reader_mailboxes("concierge=inbox-append.sh"), vec!["concierge"]);
        assert_eq!(parse_reader_mailboxes(" a=x.sh\nb=y.sh  c "), vec!["a", "b", "c"]);
        assert!(parse_reader_mailboxes("").is_empty());
    }
}
