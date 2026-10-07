//! Environment bootstrap (wave4-decomposition.md row C4; bead sp-kfimz, "wave 4.6:
//! environment bootstrap into spira-config") — the PATH tail, `SPIRA_BD` resolution and the
//! cached bd-schema preflight `conf.sh` used to run inline, in bash, every time it was
//! sourced. `resolve()`'s own module doc (sp-eekjm, "wave 4.4") named these three as
//! deliberately out of its scope; this module is where they landed instead, so the family
//! exists exactly once rather than in bash and in a Rust re-implementation that could drift
//! from it.
//!
//! `conf.sh` now calls the CLI surface in `main.rs` (`env-bootstrap --sh`, `check-bd`) and
//! acts only on exit status plus the `eval`-able lines on stdout — it no longer carries any
//! of this logic itself. Every refusal wording, the cache's own key (the `bd` binary's
//! mtime+size, not its version string: `bd`'s version does not order against its migration
//! count, the sp-s2zvn scar `check_bd_schema`'s own doc below explains), and the
//! `SPIRA_DOCTOR` self-exemption are preserved exactly, byte for byte, because at least one
//! suite (`test-bd-schema-stamp.sh`) asserts on the exact message text and on the stamp
//! file's own presence.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

/// THE LAUNCHER'S PATH COMES FIRST AND IS NEVER REWRITTEN (sp-gypjk, law). `current` is the
/// PATH already in force — the launcher put the release's `bin/` and `spira/` at the front —
/// and this function only ever APPENDS a tail: `spira_path` (`SPIRA_PATH`, colon-separated,
/// the box's own setting), then `<home>/.local/bin`, `<home>/.cargo/bin` (cargo, for the
/// gate/testenv that build a tree under test — it holds no Spira tool), then the system
/// directories, each segment added at most once. Mirrors the bash loop's own
/// `case ":${PATH:-}:" in *":$seg:"*)` test, which checks against the PATH assembled SO FAR
/// on every iteration, not only against the original `current` — so a segment named twice in
/// `spira_path` itself, or one that collides with `<home>/.cargo/bin`, is still added once.
/// Calling this again (conf.sh re-sourced in the same shell) is idempotent: nothing already
/// present is re-added, so PATH never grows without bound.
pub fn path_tail(current: &str, spira_path: &str, home: &str) -> String {
    let mut path = current.to_string();
    let mut segs: Vec<String> = spira_path
        .split(':')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    segs.push(format!("{home}/.local/bin"));
    segs.push(format!("{home}/.cargo/bin"));
    segs.push("/usr/local/bin".to_string());
    segs.push("/usr/bin".to_string());
    segs.push("/bin".to_string());
    for seg in segs {
        let needle = format!(":{seg}:");
        let probe = format!(":{path}:");
        if !probe.contains(&needle) {
            path = if path.is_empty() { seg } else { format!("{path}:{seg}") };
        }
    }
    path
}

/// `SPIRA_BD` — resolved once, deterministically, from `path` (the PATH [`path_tail`] just
/// assembled, never whatever PATH the calling context happened to carry in). sp-s2zvn's
/// scar: a bare `${SPIRA_BD:-bd}` fallback in lib.sh picked a different binary depending on
/// whether the caller was a login shell, a systemd unit or an aeon's confined environment —
/// all three disagree about PATH order. Mirrors `command -v bd`'s own search: the first `bd`
/// on `path` that is a regular file with any execute bit set; an empty PATH segment means
/// the current directory, exactly as the shell resolves one. Falls back to the bare name
/// `"bd"` when nothing is found, so invoking it later fails loudly (`bd: command not found`)
/// instead of silently running the wrong binary.
pub fn resolve_bd(path: &str) -> String {
    for dir in path.split(':') {
        let dir = if dir.is_empty() { "." } else { dir };
        let candidate = Path::new(dir).join("bd");
        if is_executable_file(&candidate) {
            return candidate.to_string_lossy().into_owned();
        }
    }
    "bd".to_string()
}

fn is_executable_file(p: &Path) -> bool {
    match fs::metadata(p) {
        Ok(m) => m.is_file() && (m.permissions().mode() & 0o111 != 0),
        Err(_) => false,
    }
}

/// `env-bootstrap --sh`'s whole output (`main.rs`) — `conf.sh`'s own `eval` target for this
/// bead, the same `KEY='value'` shape `resolve --sh`/`--sh-all` already use. Both names are
/// `export`ed inline here (unlike `Resolved::to_sh_all`'s bare assignments) because `PATH`
/// and `SPIRA_BD` must reach THIS shell's own already-sourced code (and any further child it
/// spawns before reaching `conf.sh`'s own later, explicit export list) immediately, not only
/// once that later block runs.
///
/// `existing_bd`: the caller's/config's own prior value, if any (`conf.sh` passes
/// `${SPIRA_BD:-}` — the environment and the config file both outrank derivation, exactly as
/// bash's own `[ -z "${SPIRA_BD:-}" ]` guard did). A non-empty value survives untouched;
/// [`resolve_bd`] runs only when it is empty.
pub fn bootstrap_sh(current_path: &str, spira_path: &str, home: &str, existing_bd: &str) -> String {
    let new_path = path_tail(current_path, spira_path, home);
    let bd = if existing_bd.trim().is_empty() {
        resolve_bd(&new_path)
    } else {
        existing_bd.to_string()
    };
    format!(
        "PATH={}\nexport PATH\nSPIRA_BD={}\nexport SPIRA_BD\n",
        crate::shell_quote(&new_path),
        crate::shell_quote(&bd),
    )
}

/// One `check-bd` outcome: the stderr lines `conf.sh`'s own CLI wrapper prints verbatim, and
/// whether the caller must refuse (`exit 1` — never `return`: a schema mismatch ends the
/// whole process that sourced `conf.sh`, exactly as the bash this replaces always did, so
/// that a handful of callers sourcing `conf.sh` as `. conf.sh || true` still cannot shrug
/// this one off; see `conf.sh`'s own comment on that `||` guard).
pub struct SchemaCheckResult {
    pub messages: Vec<String>,
    pub refuse: bool,
}

impl SchemaCheckResult {
    fn ok(messages: Vec<String>) -> Self {
        SchemaCheckResult { messages, refuse: false }
    }
    fn refuse(messages: Vec<String>) -> Self {
        SchemaCheckResult { messages, refuse: true }
    }
}

/// The bd schema preflight (wave4-decomposition.md (c) #6, "conf.sh's own side effects").
/// When the resolved `bd`'s migration count disagrees with the database's, `bd` exits 0 WITH
/// THE COMPLAINT ON STDOUT — a caller that only checks exit status reads success and parses
/// the error as data; this is the one place that mismatch is caught before it reaches every
/// later `bdq` call. Runs only when `<db>/.beads` exists (a fresh install, or a test fixture
/// that has not yet called `testdb_up`, has no store to check at all — the check is skipped
/// entirely, not refused). Cached by the `bd` binary's own mtime+size in
/// `<run>/bd-schema-stamp`: `bd`'s version string does NOT order against its migration count
/// (a dev build from main knows MORE migrations than a tagged release — sp-s2zvn's/the
/// BD_IGNORE_SCHEMA_SKEW scar's own lesson), so the cache is keyed on the binary's content,
/// never its reported version. `mtime` alone has only second-level resolution, so size is
/// required too: two distinct binaries built in the same second, with different content,
/// can share an mtime.
///
/// `doctor`: true when the CALLER IS doctor (`SPIRA_DOCTOR` set) — doctor runs its own
/// schema check, so a failure here is reported but TOLERATED rather than refused. Either
/// way, the stamp is written only after an OK outcome (`bd` exited 0, or the one known-safe
/// deprecation warning matched — see below), so a tolerated failure keeps re-checking, and
/// re-opening the store, on every single source until it actually clears.
pub fn check_bd_schema(bd: &str, db: &str, run: &str, doctor: bool, conf_file: &str) -> SchemaCheckResult {
    let beads_dir = format!("{db}/.beads");
    if !Path::new(&beads_dir).is_dir() {
        return SchemaCheckResult::ok(Vec::new());
    }

    let stamp_path = format!("{run}/bd-schema-stamp");
    let stamp_val = fs::metadata(bd).ok().map(|m| format!("{} {}", m.mtime(), m.len()));

    if let Some(want) = &stamp_val {
        if let Ok(have) = fs::read_to_string(&stamp_path) {
            if have.trim_end() == want {
                return SchemaCheckResult::ok(Vec::new());
            }
        }
    }

    // `timeout 30 "$bd" -C "$db" migrate schema 2>&1`, run through a shell so stderr folds
    // into the same capture bash's own `2>&1` produced — the mismatch message, the lock
    // message and a plain "command not found" all arrive on the one stream this reads.
    let script = format!(
        "timeout 30 {} -C {} migrate schema 2>&1",
        crate::shell_quote(bd),
        crate::shell_quote(db),
    );
    let (rc, combined) = match crate::bounded::bounded("/bin/sh").arg("-c").arg(&script).output() {
        Ok(o) => (o.status.code().unwrap_or(1), String::from_utf8_lossy(&o.stdout).into_owned()),
        Err(e) => (1, format!("spira: could not run {bd}: {e}")),
    };

    let mut ok = false;
    let mut refuse = false;
    let mut messages = Vec::new();

    if rc == 0 {
        ok = true;
    } else if dolt_server_port_deprecated(&combined) {
        // A deprecated field in metadata.json; schema is unaffected (sp-lh8r).
        ok = true;
    } else if combined.contains("locked by another dolt process") {
        // Lock contention means the store is in embedded mode — one exclusive lock, many
        // waiters. Embedded mode is refused by doctor; run it to diagnose.
        messages.push("spira: bd locked — store is in embedded mode (dolt_mode); run doctor".to_string());
        refuse = !doctor;
    } else {
        let db_v = extract_version(&combined, "database is at v");
        let bin_v = extract_version(&combined, "binary knows up to v");
        if db_v.is_some() || bin_v.is_some() {
            messages.push(format!(
                "spira: bd schema mismatch — database is at v{}, {} knows up to v{}",
                db_v.as_deref().unwrap_or("?"),
                bd,
                bin_v.as_deref().unwrap_or("?"),
            ));
            messages.push(format!(
                "spira: rebuild bd at v{} or set SPIRA_BD in {}",
                db_v.as_deref().unwrap_or("?"),
                conf_file,
            ));
        } else {
            let first_line = combined.lines().next().unwrap_or("");
            messages.push(format!("spira: bd migrate schema failed — {first_line}"));
            messages.push(format!("spira: bd is {bd}"));
        }
        refuse = !doctor;
    }

    // Write the stamp only after a successful check (best-effort; failure is silent, exactly
    // as the bash `|| true` this replaces was).
    if ok {
        if let Some(stamp) = &stamp_val {
            if fs::create_dir_all(run).is_ok() {
                let _ = fs::write(&stamp_path, format!("{stamp}\n"));
            }
        }
    }

    if refuse {
        SchemaCheckResult::refuse(messages)
    } else {
        SchemaCheckResult::ok(messages)
    }
}

/// `grep -q 'dolt_server_port.*deprecated'` — a per-line basic-regex match, not a whole-text
/// substring search: `deprecated` must appear on the SAME line as `dolt_server_port`, at or
/// after it.
fn dolt_server_port_deprecated(out: &str) -> bool {
    out.lines().any(|l| {
        l.find("dolt_server_port")
            .map(|idx| l[idx..].contains("deprecated"))
            .unwrap_or(false)
    })
}

/// `printf '%s\n' "$out" | grep -oE '<marker>[0-9]+' | grep -oE '[0-9]+'` — the digits
/// immediately following `marker`'s first occurrence, or `None` if `marker` never appears or
/// is followed by no digit at all.
fn extract_version(out: &str, marker: &str) -> Option<String> {
    let idx = out.find(marker)?;
    let rest = &out[idx + marker.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_tail_appends_each_segment_once() {
        let got = path_tail("/release/bin:/release/spira", "/opt/custom", "/fixture-home");
        assert_eq!(
            got,
            "/release/bin:/release/spira:/opt/custom:/fixture-home/.local/bin:/fixture-home/.cargo/bin:/usr/local/bin:/usr/bin:/bin"
        );
    }

    #[test]
    fn path_tail_never_shadows_the_launchers_release() {
        // THE LAUNCHER'S PATH COMES FIRST AND IS NEVER REWRITTEN (sp-gypjk): a tail segment
        // that already appears at the front (here, SPIRA_PATH naming /usr/bin, which is also
        // a hardcoded tail entry) must not be duplicated or reordered.
        let got = path_tail("/release/bin", "/usr/bin", "/fixture-home");
        assert_eq!(got, "/release/bin:/usr/bin:/fixture-home/.local/bin:/fixture-home/.cargo/bin:/usr/local/bin:/bin");
    }

    #[test]
    fn path_tail_is_idempotent_on_resource() {
        let once = path_tail("/release/bin", "", "/fixture-home");
        let twice = path_tail(&once, "", "/fixture-home");
        assert_eq!(once, twice, "re-sourcing conf.sh must not grow PATH");
    }

    #[test]
    fn path_tail_empty_spira_path_contributes_nothing() {
        let got = path_tail("/release/bin", "", "/fixture-home");
        assert_eq!(got, "/release/bin:/fixture-home/.local/bin:/fixture-home/.cargo/bin:/usr/local/bin:/usr/bin:/bin");
    }

    #[test]
    fn path_tail_collapses_doubled_colons_in_spira_path() {
        let got = path_tail("", "a::b", "/h");
        assert_eq!(got, "a:b:/h/.local/bin:/h/.cargo/bin:/usr/local/bin:/usr/bin:/bin");
    }

    #[test]
    fn resolve_bd_finds_the_first_executable_on_path() {
        let dir = testkit::TempDir::new("spira-config-env-bootstrap-resolve-bd");
        let bd = dir.path().join("bd");
        testkit::write_exe(&bd, "#!/bin/sh\nexit 0\n");
        let path = format!("/nonexistent:{}:/usr/bin", dir.path().display());
        assert_eq!(resolve_bd(&path), bd.to_string_lossy());
    }

    #[test]
    fn resolve_bd_skips_a_non_executable_candidate() {
        let dir = testkit::TempDir::new("spira-config-env-bootstrap-resolve-bd-noexec");
        fs::write(dir.path().join("bd"), "not executable").unwrap();
        let path = format!("{}:/usr/bin", dir.path().display());
        // /usr/bin/bd does not exist in this test environment either, so the fallback fires.
        assert_eq!(resolve_bd(&path), "bd");
    }

    #[test]
    fn resolve_bd_falls_back_to_bare_name_when_nothing_found() {
        assert_eq!(resolve_bd("/nonexistent/one:/nonexistent/two"), "bd");
    }

    #[test]
    fn bootstrap_sh_leaves_an_existing_spira_bd_untouched() {
        let out = bootstrap_sh("/release/bin", "", "/fixture-home", "/pinned/bd");
        assert!(out.contains("SPIRA_BD='/pinned/bd'"), "{out}");
    }

    #[test]
    fn bootstrap_sh_resolves_bd_when_unset() {
        // Hermetic regardless of the host/container: path_tail's fixed tail always ends in
        // real system directories (/usr/local/bin, /usr/bin, /bin), and the testenv
        // container genuinely ships a `bd` at /usr/local/bin/bd (spira/testenv/Containerfile)
        // for the suites' own use. A literal `"bd"` fallback assertion here is true on a bare
        // host but false inside that container, which finds the real one first — not a bug,
        // just a second real `bd` earlier in this test's search than the author pictured.
        // Plant a controlled, uniquely-named executable in `<home>/.cargo/bin`, which
        // path_tail places ahead of every hardcoded system directory, so resolution is
        // deterministic no matter what else is installed on the box.
        let dir = testkit::TempDir::new("spira-config-env-bootstrap-bootstrap-sh-unset");
        let cargo_bin = dir.path().join(".cargo/bin");
        fs::create_dir_all(&cargo_bin).unwrap();
        let bd = cargo_bin.join("bd");
        testkit::write_exe(&bd, "#!/bin/sh\nexit 0\n");

        let out = bootstrap_sh("/release/bin", "", &dir.path().to_string_lossy(), "");
        assert!(out.contains(&format!("SPIRA_BD='{}'", bd.to_string_lossy())), "{out}");
        assert!(out.starts_with("PATH="), "{out}");
    }

    #[test]
    fn check_bd_schema_skips_when_no_store() {
        let dir = testkit::TempDir::new("spira-config-check-bd-no-store");
        let run = dir.path().join("run");
        let result = check_bd_schema("bd", &dir.path().join("no-such-db").to_string_lossy(), &run.to_string_lossy(), false, "spira.conf");
        assert!(!result.refuse);
        assert!(result.messages.is_empty());
        assert!(!run.exists(), "a skipped check must not even create SPIRA_RUN");
    }

    fn setup_store(dir: &Path) -> String {
        let db = dir.join("db");
        fs::create_dir_all(db.join(".beads")).unwrap();
        db.to_string_lossy().into_owned()
    }

    #[test]
    fn check_bd_schema_passes_and_stamps_on_a_clean_bd() {
        let dir = testkit::TempDir::new("spira-config-check-bd-ok");
        let db = setup_store(dir.path());
        let run = dir.path().join("run");
        let bd = dir.path().join("bd");
        testkit::write_exe(&bd, "#!/bin/sh\nprintf 'Schema already at v61\\n'\nexit 0\n");

        let result = check_bd_schema(&bd.to_string_lossy(), &db, &run.to_string_lossy(), false, "spira.conf");
        assert!(!result.refuse, "{:?}", result.messages);
        assert!(run.join("bd-schema-stamp").is_file(), "stamp must be written on success");
    }

    #[test]
    fn check_bd_schema_caches_so_a_second_call_never_runs_bd() {
        let dir = testkit::TempDir::new("spira-config-check-bd-cache");
        let db = setup_store(dir.path());
        let run = dir.path().join("run");
        let call_log = dir.path().join("calls");
        let bd = dir.path().join("bd");
        testkit::write_exe(
            &bd,
            &format!(
                "#!/bin/sh\nn=$(cat {log} 2>/dev/null || echo 0)\necho $((n+1)) > {log}\nprintf 'Schema already at v61\\n'\nexit 0\n",
                log = call_log.display(),
            ),
        );

        let r1 = check_bd_schema(&bd.to_string_lossy(), &db, &run.to_string_lossy(), false, "spira.conf");
        assert!(!r1.refuse);
        let r2 = check_bd_schema(&bd.to_string_lossy(), &db, &run.to_string_lossy(), false, "spira.conf");
        assert!(!r2.refuse);
        assert_eq!(fs::read_to_string(&call_log).unwrap().trim(), "1", "second call must be a cache hit");
    }

    #[test]
    fn check_bd_schema_refuses_on_mismatch_and_does_not_stamp() {
        let dir = testkit::TempDir::new("spira-config-check-bd-mismatch");
        let db = setup_store(dir.path());
        let run = dir.path().join("run");
        let bd = dir.path().join("bd");
        testkit::write_exe(
            &bd,
            "#!/bin/sh\nprintf 'database is at v99\\nbinary knows up to v53\\n' >&2\nexit 1\n",
        );

        let result = check_bd_schema(&bd.to_string_lossy(), &db, &run.to_string_lossy(), false, "spira.conf");
        assert!(result.refuse);
        assert!(result.messages.iter().any(|m| m.contains("schema mismatch")), "{:?}", result.messages);
        assert!(result.messages.iter().any(|m| m.contains("v99") && m.contains("v53")), "{:?}", result.messages);
        assert!(!run.join("bd-schema-stamp").exists(), "a refused check must not stamp success");
    }

    #[test]
    fn check_bd_schema_doctor_tolerates_mismatch_without_refusing() {
        let dir = testkit::TempDir::new("spira-config-check-bd-doctor");
        let db = setup_store(dir.path());
        let run = dir.path().join("run");
        let bd = dir.path().join("bd");
        testkit::write_exe(
            &bd,
            "#!/bin/sh\nprintf 'database is at v99\\nbinary knows up to v53\\n' >&2\nexit 1\n",
        );

        let result = check_bd_schema(&bd.to_string_lossy(), &db, &run.to_string_lossy(), true, "spira.conf");
        assert!(!result.refuse, "doctor runs its own schema check and must not be halted here");
        assert!(result.messages.iter().any(|m| m.contains("schema mismatch")), "doctor still sees the message");
        assert!(!run.join("bd-schema-stamp").exists(), "a tolerated failure must re-check next time, not cache");
    }

    #[test]
    fn check_bd_schema_tolerates_the_known_dolt_server_port_deprecation() {
        let dir = testkit::TempDir::new("spira-config-check-bd-deprecated");
        let db = setup_store(dir.path());
        let run = dir.path().join("run");
        let bd = dir.path().join("bd");
        testkit::write_exe(
            &bd,
            "#!/bin/sh\nprintf 'warning: dolt_server_port is deprecated\\n'\nexit 1\n",
        );

        let result = check_bd_schema(&bd.to_string_lossy(), &db, &run.to_string_lossy(), false, "spira.conf");
        assert!(!result.refuse, "{:?}", result.messages);
        assert!(run.join("bd-schema-stamp").is_file(), "the deprecation case is still an OK outcome");
    }

    #[test]
    fn check_bd_schema_refuses_on_lock_contention_naming_doctor() {
        let dir = testkit::TempDir::new("spira-config-check-bd-locked");
        let db = setup_store(dir.path());
        let run = dir.path().join("run");
        let bd = dir.path().join("bd");
        testkit::write_exe(
            &bd,
            "#!/bin/sh\nprintf 'database is locked by another dolt process\\n'\nexit 1\n",
        );

        let result = check_bd_schema(&bd.to_string_lossy(), &db, &run.to_string_lossy(), false, "spira.conf");
        assert!(result.refuse);
        assert!(result.messages.iter().any(|m| m.contains("embedded mode") && m.contains("doctor")), "{:?}", result.messages);
    }
}
