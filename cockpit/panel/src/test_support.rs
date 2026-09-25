//! A throwaway `bd` on `SPIRA_PATH`, for gap #3 of
//! docs/test-plan/cockpit-observability.md: model.rs's `close_decision`, `comment`, `run_as`
//! and the enact path, plus store.rs's `refresh`, are the only cockpit surfaces that change
//! bead state, and none of them had ever run against anything but the real `bd`.
//!
//! `bin()` (store.rs) resolves `bd` by searching `SPIRA_PATH` first, so pointing that at a
//! directory holding a recording stub is the whole seam — no code under test changes.
//!
//! ENV VARS ARE PROCESS-GLOBAL, so every test using this must hold `LOCK` for as long as the
//! stub is in scope. `StubBd` does that itself (the guard lives in the struct) — a caller
//! only has to keep the `StubBd` alive, not remember the lock.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

pub static LOCK: Mutex<()> = Mutex::new(());
static COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct StubBd {
    dir: std::path::PathBuf,
    log: std::path::PathBuf,
    set_vars: Vec<String>,
    saved_spira_path: Option<String>,
    _guard: MutexGuard<'static, ()>,
}

impl StubBd {
    /// Installs the stub and takes the env lock. Panics on setup failure — a test fixture
    /// that cannot build is a broken test, not a case to assert on.
    pub fn new() -> Self {
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("panel-stub-bd-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create stub bd dir");
        let log = dir.join("argv.log");
        std::fs::write(&log, "").expect("init argv log");
        let script = dir.join("bd");
        std::fs::write(
            &script,
            format!(
                r#"#!/usr/bin/env bash
{{ printf 'ARGV: %s\n' "$*"; printf 'ACTOR: %s\n' "${{BEADS_ACTOR:-<none>}}"; }} >> {log:?}
case " $* " in
    *" close "*)
        rc="${{BD_CLOSE_RC:-0}}"; err="${{BD_CLOSE_ERR:-}}"; out="${{BD_CLOSE_OUT:-}}" ;;
    *" comments add "*)
        rc="${{BD_COMMENTS_RC:-0}}"; err="${{BD_COMMENTS_ERR:-}}"; out="${{BD_COMMENTS_OUT:-}}" ;;
    *" comments "*)
        rc="${{BD_COMMENTS_LIST_RC:-0}}"; err="${{BD_COMMENTS_LIST_ERR:-}}"; out="${{BD_COMMENTS_LIST_OUT:-[]}}" ;;
    *" list "*)
        rc="${{BD_LIST_RC:-0}}"; err="${{BD_LIST_ERR:-}}"
        # No explicit BD_LIST_OUT: succeed with an empty list, or fail with EMPTY stdout —
        # store.rs's run() only treats a nonzero exit as an error when stdout is empty too,
        # the same "failed but printed something" shape a real bd can produce.
        if [ -n "${{BD_LIST_OUT:-}}" ]; then
            out="$BD_LIST_OUT"
        elif [ "$rc" = "0" ]; then
            out='{{"issues":[]}}'
        else
            out=""
        fi ;;
    *)
        rc=0; err=""; out="" ;;
esac
[ -n "$out" ] && printf '%s' "$out"
[ -n "$err" ] && printf '%s\n' "$err" >&2
exit "$rc"
"#
            ),
        )
        .expect("write stub bd script");
        let mut perm = std::fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
        std::fs::set_permissions(&script, perm).expect("chmod stub bd");

        let saved_spira_path = std::env::var("SPIRA_PATH").ok();
        std::env::set_var("SPIRA_PATH", &dir);
        Self {
            dir,
            log,
            set_vars: Vec::new(),
            saved_spira_path,
            _guard: guard,
        }
    }

    /// Sets an env var for the stub's lifetime; removed on drop.
    pub fn env(mut self, k: &str, v: &str) -> Self {
        std::env::set_var(k, v);
        self.set_vars.push(k.to_string());
        self
    }

    pub fn argv_log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for StubBd {
    fn drop(&mut self) {
        for k in &self.set_vars {
            std::env::remove_var(k);
        }
        match &self.saved_spira_path {
            Some(p) => std::env::set_var("SPIRA_PATH", p),
            None => std::env::remove_var("SPIRA_PATH"),
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
