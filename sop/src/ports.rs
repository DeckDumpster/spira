//! Every external dependency `sop` has, as traits. `logic.rs` is otherwise pure: a test
//! drives it through fakes, the way `gh-intake`'s tests do (DESIGN.md §"Ports" — this
//! replaces the bash's env-var seams, `SOP_SHELF_CMD` and `GH_INTAKE_LIB`'s sibling, with
//! real dependency injection, law-prefer-the-real-dependency).

/// The beads store, reached through `lib.sh`'s `bdq`/`bdjson` — NOT called directly.
/// `bdq` carries real safety logic this rewrite does not touch: a repo-label vocabulary
/// check, a destructive-SQL refusal, the czar-fence hook, and a retry around a Dolt
/// connection the server already dropped (`spira/lib.sh` around `bdq()`). `spira/lib.sh` is
/// last in the rewrite order (the inventory, group 4) and Ryan's standing instruction during
/// the cutover was "leave lib.sh alone" — so every call here shells out to it exactly as
/// `gate-check`'s Rust port already shells out to `repo_root` (same seam, same reason).
///
/// NOT COLLAPSED onto a direct `bdq`-binary call (sp-pwmlj, wave 4.15 — considered and
/// rejected). `lib.sh`'s own `bdq`/`bdjson` are already one-line shims onto `bead::bdq`
/// (sp-w3h16), so `real.rs`'s seam has no competing Rust logic to retire — it reaches the
/// one real implementation already. The bash hop still earns its keep here: none of
/// `remember`/`recall`/`forget`/`memories_json`/`note` ever call `bd create`, so the three
/// create-time fences never fire, but a fresh `conf.sh` sourcing is still how `SPIRA_DB`
/// reaches this process at all — this crate's own contract (DESIGN.md §3) is "`SPIRA_DB`
/// implicitly, via `lib.sh`", not a value `sop`'s own caller is guaranteed to export.
/// Skipping the sourcing would need that resolved in-process first (wave4-decomposition.md
/// rows 4–6, not yet landed).
pub trait Bd {
    /// `bdq remember --key <key> <text>`.
    fn remember(&self, key: &str, text: &str) -> bool;
    /// `bdq recall <key>`. `None` when recall fails (no such key, or bd unreachable) —
    /// the two are not distinguished here because `show`'s caller does not need to.
    fn recall(&self, key: &str) -> Option<String>;
    /// `bdq forget <key>`.
    fn forget(&self, key: &str) -> bool;
    /// `bdjson memories`: the raw JSON text of every remembered key. `None` when the call
    /// produced no parseable bytes — the shelf-unreadable case every subcommand here treats
    /// as distinct from a genuinely empty shelf (law-absence-needs-a-positive-control).
    fn memories_json(&self) -> Option<String>;
    /// `bdq note <bead> --stdin`, body on stdin.
    fn note(&self, bead: &str, text: &str) -> bool;
}

/// Everything else `sop` shells out to: `spira-lint --only inventory --scan <path>` for the
/// operator-infrastructure check, and a cockpit metric probe for METRIC enforcement.
pub trait Proc {
    /// `spira-lint --only inventory --scan /dev/stdin`, `text` on stdin. Returns each
    /// offending line spira-lint printed (empty = clean). `Err` when spira-lint itself
    /// could not be run — fail closed, never read as clean.
    fn inventory_scan(&self, text: &str) -> Result<Vec<String>, String>;

    /// The METRIC probe: `timeout <secs> <bin> probe <subcmd>` by default (cockpit-
    /// collect's own subcommand, sp-kt4l3), or `timeout <secs> bash <bin> <subcmd>` when
    /// `bash_prefix` is set. The bash prefix is not a guess from `bin`'s name: the
    /// original script's `${SOP_METRIC_COCKPIT:+bash}` prefixes with `bash` exactly when
    /// the caller OVERRODE the cockpit binary (`SOP_METRIC_COCKPIT` set to anything) — an
    /// override runs bare, taking the subcmd directly, same as before cockpit-collect
    /// existed — and never for the default, which `real.rs` inserts the `probe` arg for.
    /// The caller, which knows whether an override was given, decides `bash_prefix`, not
    /// `real.rs`.
    /// Returns raw stdout, or `None` on any failure (missing binary, non-zero exit,
    /// timeout) — `applied` treats `None` exactly like an unreadable metric.
    fn metric_probe(&self, bin: &str, bash_prefix: bool, subcmd: &str, timeout_secs: u64) -> Option<String>;
}

/// One clock read for `applied`'s timestamp, and `synth`'s "as of" date — both a single read
/// (never two `date` calls that could straddle a second boundary), and both the seam a test
/// fixes.
pub trait Clock {
    /// `(unix epoch, ISO-8601 UTC "YYYY-MM-DDTHH:MM:SSZ")`, from ONE read.
    fn now(&self) -> (u64, String);
    /// Today's date, `YYYY-MM-DD`, in `SPIRA_TZ` (default `TZ`, else UTC) — `synth`'s
    /// "as of" line.
    fn today(&self) -> String;
}
