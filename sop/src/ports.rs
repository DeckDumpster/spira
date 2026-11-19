//! Every external dependency `sop` has, as traits. `logic.rs` is otherwise pure: a test
//! drives it through fakes, the way `gh-intake`'s tests do (DESIGN.md §"Ports" — this
//! replaces the bash's env-var seams, `SOP_SHELF_CMD` and `GH_INTAKE_LIB`'s sibling, with
//! real dependency injection, law-prefer-the-real-dependency).

/// The beads store, reached only through the content door (`spira-lc content`).
pub trait Bd {
    /// `content remember --key <key>`, text on stdin.
    fn remember(&self, key: &str, text: &str) -> Result<(), String>;
    /// `content recall <key>`. `None` when recall fails (no such key, or bd unreachable) —
    /// the two are not distinguished here because `show`'s caller does not need to.
    fn recall(&self, key: &str) -> Option<String>;
    /// `content forget <key>`.
    fn forget(&self, key: &str) -> bool;
    /// `content memories --json`: the raw JSON text of every remembered key. `None` when the call
    /// produced no parseable bytes — the shelf-unreadable case every subcommand here treats
    /// as distinct from a genuinely empty shelf (law-absence-needs-a-positive-control).
    fn memories_json(&self) -> Option<String>;
    /// `content note <bead> --stdin`, body on stdin.
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
    /// Today's date, `YYYY-MM-DD`, in `SPIRA_TZ`'s declared value — `synth`'s "as of" line.
    fn today(&self) -> String;
}
