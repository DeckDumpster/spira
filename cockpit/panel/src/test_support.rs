//! A throwaway `bd` on a stub directory, for gap #3 of
//! docs/test-plan/cockpit-observability.md: model.rs's `close_decision`, `comment`, `run_as`
//! and the enact path, plus store.rs's `refresh`, are the only cockpit surfaces that change
//! bead state, and none of them had ever run against anything but the real `bd`.
//!
//! `store::bin`/`store::child_path` resolve `bd` by searching `cfg.extra_path` first, so
//! pointing that at a directory holding a recording stub is the whole seam — no code under
//! test changes. Per Ryan 2026-10-05 (one source of config), this no longer mutates
//! `SPIRA_PATH`/`COCKPIT_DB`/`SPIRA_DB`/`SPIRA_OPERATOR_ACTOR`: `spira_config::process::cfg`
//! resolves those keys from `$SPIRA_TOML` ONCE per process and caches the answer forever, so a
//! test that varied them through the environment would only ever affect whichever test ran
//! first in this shared test binary. `StubBd::cfg()` hands back an explicit `store::Cfg`
//! instead, built from this stub's own fields, for the caller to pass into the function under
//! test directly — the same "pure logic takes config as arguments" rule the production call
//! sites (`App`, built once in `main`) now follow too.
//!
//! ENV VARS ARE STILL PROCESS-GLOBAL for every OTHER knob here (`BD_CLOSE_RC`, `MAIL_RC`,
//! `LC_RC`, `RULE_RC`, `SPIRA_RULE`, …) — those are read by the stub shell scripts themselves,
//! or by `model::rule_sh` (not a registered config key), never by `spira_config`. Every test
//! using this must still hold `LOCK` for as long as the stub is in scope; `StubBd` does that
//! itself (the guard lives in the struct) — a caller only has to keep the `StubBd` alive, not
//! remember the lock.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

pub static LOCK: Mutex<()> = Mutex::new(());
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// The stub `spira-lc`. `close` is the decision close (sp-3fue0j: it goes through the
/// lifecycle machine, never bd): logged as the closing bd used to log it — `ARGV: close <id>
/// --reason <stdin> --force` and `ACTOR: <--actor>` — and answering `BD_CLOSE_RC`/`BD_CLOSE_ERR`.
/// Every other verb logs `LC: <argv>` once [`StubBd::lc`] armed it, and exits `LC_RC`.
fn write_lc(dir: &std::path::Path, log: &std::path::Path) {
    let armed = dir.join("lc.armed");
    testkit::write_exe(
        &dir.join("spira-lc"),
        &format!(
            r#"#!/usr/bin/env bash
if [ "$1" = close ]; then
    id="$2"; actor=""; shift 2
    while [ $# -gt 0 ]; do case "$1" in --actor) actor="$2"; shift 2 ;; *) shift ;; esac; done
    reason="$(cat)"
    {{ printf 'ARGV: close %s --reason %s --force\n' "$id" "$reason"; printf 'ACTOR: %s\n' "$actor"; }} >> {log:?}
    [ -n "${{BD_CLOSE_ERR:-}}" ] && printf '%s\n' "$BD_CLOSE_ERR" >&2
    exit "${{BD_CLOSE_RC:-0}}"
fi
[ -f {armed:?} ] && printf 'LC: %s\n' "$*" >> {log:?}
exit "${{LC_RC:-0}}"
"#
        ),
    );
}

pub struct StubBd {
    dir: testkit::TempDir,
    log: std::path::PathBuf,
    env_edits: Vec<(String, String)>,
    env_guard: Option<testkit::EnvGuard>,
    mail_inbox: Option<std::path::PathBuf>,
    /// `Cfg::db`, settable with `.db(...)`. Empty by default — most tests pass a db path
    /// straight into the function under test (e.g. `close_decision("/fake/db", …)`) and never
    /// need this; `comment`/`enact`/`refresh` read it from `Cfg` instead, so they use this.
    db: String,
    /// `Cfg::operator_actor`, settable with `.operator_actor(...)`. Defaults to "operator",
    /// the same registry default `SPIRA_OPERATOR_ACTOR` carries.
    operator_actor: String,
    _guard: MutexGuard<'static, ()>,
}

impl StubBd {
    /// Installs the stub and takes the env lock. Panics on setup failure — a test fixture
    /// that cannot build is a broken test, not a case to assert on.
    pub fn new() -> Self {
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = testkit::TempDir::new(&format!("panel-stub-bd-{n}"));
        let log = dir.join("argv.log");
        std::fs::write(&log, "").expect("init argv log");
        let script = dir.join("bd");
        // testkit::write_exe, never fs::write + set_mode/set_permissions (sp-os3of).
        testkit::write_exe(
            &script,
            &format!(
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
        );

        write_lc(&dir, &log);
        Self {
            dir,
            log,
            env_edits: Vec::new(),
            env_guard: None,
            mail_inbox: None,
            db: String::new(),
            operator_actor: "operator".to_string(),
            _guard: guard,
        }
    }

    /// Sets an env var for the stub's lifetime; removed on drop. For the stub SCRIPTS' own
    /// knobs (`BD_CLOSE_RC`, `MAIL_RC`, `LC_RC`, `RULE_RC`, …) and for `SPIRA_RULE` (not a
    /// registered config key) — never for `SPIRA_DB`/`SPIRA_OPERATOR_ACTOR`, which are
    /// `Cfg` fields now; use `.db(...)`/`.operator_actor(...)` for those.
    pub fn env(mut self, k: &str, v: &str) -> Self {
        // The testkit lock is not reentrant: release the guard (restoring) before retaking
        // it with every edit made so far.
        self.env_guard = None;
        self.env_edits.push((k.to_string(), v.to_string()));
        let edits: Vec<(&str, Option<&str>)> =
            self.env_edits.iter().map(|(k, v)| (k.as_str(), Some(v.as_str()))).collect();
        self.env_guard = Some(testkit::env(&edits));
        self
    }

    /// `Cfg::db` for the `Cfg` this stub hands back from [`StubBd::cfg`].
    pub fn db(mut self, v: &str) -> Self {
        self.db = v.to_string();
        self
    }

    /// `Cfg::operator_actor` for the `Cfg` this stub hands back from [`StubBd::cfg`].
    pub fn operator_actor(mut self, v: &str) -> Self {
        self.operator_actor = v.to_string();
        self
    }

    /// The `store::Cfg` to pass into whatever is under test: `extra_path` points at this
    /// stub's directory (where the fake `bd`/`mail`/`rule.sh`/`spira-lc` live), `db` and
    /// `operator_actor` are whatever `.db(...)`/`.operator_actor(...)` set, or the defaults.
    pub fn cfg(&self) -> crate::store::Cfg {
        crate::store::Cfg {
            db: self.db.clone(),
            extra_path: vec![self.dir.path().to_string_lossy().into_owned()],
            operator_actor: self.operator_actor.clone(),
        }
    }

    /// Also installs a `rule.sh` stub beside the `bd` one and points `SPIRA_RULE` at it, for
    /// `enact`/`enact_law`/`suit_verdict` — the panel's other bead-state-changing surface,
    /// which shells to `rule.sh` before it ever touches `bd`. Argv goes to the SAME log,
    /// prefixed `RULE:`, so a test can assert the enact-then-label ORDER, not just that both
    /// ran. Controlled by `RULE_RC` / `RULE_ERR`. `SPIRA_RULE` is not a registered config key
    /// (`model::rule_sh` reads it straight from the environment, by design), so this still
    /// mutates env rather than going through `Cfg`.
    pub fn rule(mut self) -> Self {
        let script = self.dir.join("rule.sh");
        // testkit::write_exe, never fs::write + set_mode/set_permissions (sp-os3of).
        testkit::write_exe(
            &script,
            &format!(
                r#"#!/usr/bin/env bash
printf 'RULE: %s\n' "$*" >> {log:?}
rc="${{RULE_RC:-0}}"; err="${{RULE_ERR:-}}"
[ -n "$err" ] && printf '%s\n' "$err" >&2
exit "$rc"
"#,
                log = self.log
            ),
        );
        self.env("SPIRA_RULE", script.to_str().expect("utf-8 stub path"))
    }

    /// Also installs a `mail` stub beside the `bd` one — on the stub dir `Cfg::extra_path`
    /// names, so the panel's by-name `mail` (child_path) finds it first — for
    /// `close_decision`/`comment`'s mail-delivery leg. The stub's stdin (the whole RFC 5322
    /// message) is captured to its own file, since argv alone (`sendmail`) says nothing about
    /// what was sent. Controlled by `MAIL_RC`.
    pub fn mail(mut self) -> Self {
        let script = self.dir.join("mail");
        let inbox = self.dir.join("mail-inbox");
        // testkit::write_exe, never fs::write + set_mode/set_permissions (sp-os3of).
        testkit::write_exe(
            &script,
            &format!(
                r#"#!/usr/bin/env bash
printf 'MAIL: %s\n' "$*" >> {log:?}
cat > {inbox:?}
rc="${{MAIL_RC:-0}}"; err="${{MAIL_ERR:-}}"
[ -n "$err" ] && printf '%s\n' "$err" >&2
exit "$rc"
"#,
                log = self.log,
                inbox = inbox,
            ),
        );
        self.mail_inbox = Some(inbox);
        self
    }

    /// Also installs a `spira-lc` stub on the stub dir — the verdict's hold-lifting leg
    /// (sp-v62vn follow-up). Argv goes to the same log, prefixed `LC:`; exit `LC_RC`.
    pub fn lc(self) -> Self {
        std::fs::write(self.dir.join("lc.armed"), "").expect("arm the spira-lc stub");
        self
    }

    /// The last message the stubbed `mail sendmail` received on stdin, whole.
    pub fn mail_inbox(&self) -> String {
        self.mail_inbox
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default()
    }

    pub fn argv_log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}
