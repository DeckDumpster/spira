//! A throwaway `bd` on a stub directory, for gap #3 of
//! docs/test-plan/cockpit-observability.md: model.rs's `close_decision`, `comment`, `run_as`
//! and the enact path, plus store.rs's `refresh`, are the only cockpit surfaces that change
//! bead state, and none of them had ever run against anything but the real `bd`.
//!
//! `store::bin`/`store::child_path` resolve `bd` by searching `cfg.path` first, so
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
//! `LC_RC`, `RULE_RC`, `SPIRA_RELEASE`, …) — those are read by the stub shell scripts themselves,
//! or by `model::rule_sh` (not a registered config key), never by `spira_config`. Every test
//! using this must still hold `LOCK` for as long as the stub is in scope; `StubBd` does that
//! itself (the guard lives in the struct) — a caller only has to keep the `StubBd` alive, not
//! remember the lock.
use std::sync::Mutex;

pub static LOCK: Mutex<()> = Mutex::new(());
