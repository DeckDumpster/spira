//! The write guards `bdq` applies before a write reaches bd, as a library, so the content
//! door (`spira-lc content`, served by lc-serve) applies the same ones to the same writes:
//!
//! - **The czar fence.** A `SPIRA_RUNE=czar` session with a `SPIRA_CZAR_CLASS` set may
//!   `reopen` a bead, or `update`/`close` any bead but its own trigger
//!   (`SPIRA_CZAR_TRIGGER_BEAD`), only while that class's stage
//!   (`SPIRA_CZAR_STAGE_<CLASS>`) reads `act`. Unset is shadow (law-guard-binds-the-caller: a
//!   new class starts contained).
//! - **The claimed-description check.** An `update` that edits the title or description of a
//!   bead a dwarf holds is refused unless `--force-claimed "<reason>"` acknowledges it: the
//!   builder read the bead once, at claim, and the close-time `claim_desc_hash` comparison
//!   would reopen its close.
//!
//! - **The create-time fences** (`create`/`create-form`/`q`): a `repo:` label the repo map
//!   does not hold; destructive wording without an operator ask; an ask without a question, a
//!   `Default:` and a `Class:`; and `DELETE FROM schema_migrations`, refused always.
//! - **The ask-label write fence.** `label add <ask>` / `update --add-label|--set-labels <ask>`
//!   is refused: an ask is created whole, through the create-time shape check.
//! - **The state-verb fence.** `close`, `reopen`, `defer`, `undefer` and a status-moving
//!   `update` are refused: a bead's state is its lifecycle row's. `SPIRA_BDQ_STATE_WRITE=<reason>`
//!   overrides, and the override is logged.
//!
//! All refuse with exit 1 and the text bdq printed, in bdq's order ([`write`]). Each reads the
//! caller's own context — its environment ([`Vars`]), the lifecycle row of the bead it names,
//! the ask label and the repo map — through a port ([`Ctx`]), so the guard logic is one
//! function whichever process holds the context. [`after`] is what a successful write owes
//! the lifecycle machine: a row per created bead, a terminal row for a closed one.

use crate::bdq;
use crate::claimdesc::{self, LiveClaim};
use spira_config::lc_state::Row;
use spira_config::repos::Registry;

/// The exit code both guards refuse with: bdq's `return 1`.
pub const REFUSED: i32 = 1;

/// A guard's refusal: the exit code and the stderr text, newline-terminated, bdq gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: i32,
    pub text: String,
}

/// The caller's environment as the guards read it: `None` for an unset variable.
pub type Vars<'a> = dyn Fn(&str) -> Option<String> + 'a;
/// The lifecycle row of a bead; `None` when there is none or it cannot be read (a machine
/// that cannot answer refuses nothing, as bdq's failed read refused nothing).
pub type Rows<'a> = dyn Fn(&str) -> Option<Row> + 'a;

fn nonempty(vars: &Vars, key: &str) -> String {
    vars(key).filter(|v| !v.is_empty()).unwrap_or_default()
}

/// The stage answer for `class` with the script's exit codes: 0 `act`; 1 `shadow` (the
/// default); 2 no class, or a stage that is neither.
pub fn czar_stage(class: &str, vars: &Vars) -> Result<(), Refusal> {
    let refuse = |code: i32, text: String| Err(Refusal { code, text: format!("{text}\n") });
    if class.is_empty() {
        return refuse(2, "czar-fence: class required".into());
    }
    let var = format!("SPIRA_CZAR_STAGE_{}", class.replace('-', "_").to_uppercase());
    let stage = vars(&var).filter(|v| !v.is_empty()).unwrap_or_else(|| "shadow".into());
    match stage.as_str() {
        "act" => Ok(()),
        "shadow" => refuse(1, format!("czar-fence: {class} is shadow — mutation refused ({var}=act to enable)")),
        other => refuse(2, format!("czar-fence: unknown stage {other} in {var} (shadow or act)")),
    }
}

/// The czar fence on one bd argv: whether this caller may make this write. bdq normalises
/// every fence refusal to exit 1, as the bash's bare `|| return 1` did.
pub fn czar(args: &[String], vars: &Vars) -> Result<(), Refusal> {
    let (rune, class, trigger) = (nonempty(vars, "SPIRA_RUNE"), nonempty(vars, "SPIRA_CZAR_CLASS"), nonempty(vars, "SPIRA_CZAR_TRIGGER_BEAD"));
    match crate::bdq::czar_fence_class(&rune, &class, &trigger, args) {
        None => Ok(()),
        Some(class) => czar_stage(class, vars).map_err(|r| Refusal { code: REFUSED, text: r.text }),
    }
}

/// What the claimed-description check made of a write it lets through.
#[derive(Debug, PartialEq)]
pub enum Claim {
    /// Nothing claimed is being re-described.
    Free,
    /// A claimed bead's description edit, acknowledged with `--force-claimed`: the writer
    /// re-stamps the baseline, files the note and tells the holder after bd applies it.
    Forced { id: String, claim: LiveClaim, reason: String },
}

/// The claimed-description check on one bd argv at `now` (epoch seconds).
pub fn claim(args: &[String], rows: &Rows, now: i64) -> Result<Claim, Refusal> {
    if args.first().map(String::as_str) != Some("update") {
        return Ok(Claim::Free);
    }
    let u = claimdesc::parse_update_args(&args[1..]);
    let (true, Some(id)) = (u.touches_description, u.id) else { return Ok(Claim::Free) };
    let Some(live) = claimdesc::live_claim(rows(&id).as_ref(), now) else { return Ok(Claim::Free) };
    match u.force {
        None => Err(Refusal { code: REFUSED, text: claimdesc::refusal(&id, &live) }),
        Some(reason) => Ok(Claim::Forced { id, claim: live, reason }),
    }
}

/// Everything the guards read, as ports. `who` prefixes the guards' own failure lines (bdq
/// prints `bdq: <error>`).
pub struct Ctx<'a> {
    pub who: &'a str,
    pub vars: &'a Vars<'a>,
    pub rows: &'a Rows<'a>,
    pub now: i64,
    /// `SPIRA_ASK_LABEL`, resolved only by a write that needs it.
    pub ask_label: &'a dyn Fn() -> Result<String, String>,
    /// The repo map in force, built only by a create.
    pub registry: &'a dyn Fn() -> Registry,
}

fn refused(text: String) -> Refusal {
    Refusal { code: REFUSED, text }
}

fn ask_label(ctx: &Ctx) -> Result<String, Refusal> {
    (ctx.ask_label)().map_err(|e| refused(format!("{}: {e}\n", ctx.who)))
}

/// The create-time fences, short-circuiting on the first refusal as bdq does: repo label,
/// destructive wording, ask shape, schema delete. `body` is the create's `--body-file` text: the
/// ask-shape fence reads it as the description it is (unless `--description` is given); the
/// destructive fence judges only `--description`, since a body carries quoted evidence.
pub fn create(args: &[String], body: Option<&str>, ctx: &Ctx) -> Result<(), Refusal> {
    if !bdq::is_create(args) {
        return Ok(());
    }
    if let Some(msg) = bdq::check_repo_label(args, &(ctx.registry)()) {
        return Err(refused(msg));
    }
    let ask = ask_label(ctx)?;
    if let Some(msg) = bdq::check_destructive(args, &ask) {
        return Err(refused(msg));
    }
    let judged: Vec<String> = match body {
        Some(t) if !args.iter().any(|a| a == "--description" || a.starts_with("--description=")) => args.iter().cloned().chain(["--description".to_string(), t.to_string()]).collect(),
        _ => args.to_vec(),
    };
    if let Some(msg) = bdq::check_ask_shape(&judged, &ask) {
        return Err(refused(msg));
    }
    match bdq::check_schema_delete(args) {
        Some(msg) => Err(refused(msg)),
        None => Ok(()),
    }
}

/// The ask-label write fence on `label` and `update`.
pub fn ask_label_write(args: &[String], ctx: &Ctx) -> Result<(), Refusal> {
    if !matches!(args.first().map(String::as_str), Some("label" | "update")) {
        return Ok(());
    }
    match bdq::check_ask_label_write(args, &ask_label(ctx)?) {
        Some(msg) => Err(refused(msg)),
        None => Ok(()),
    }
}

/// The state-verb fence. `Ok(Some(line))` is an overridden write: the line is logged.
pub fn state_verb(args: &[String], vars: &Vars, who: &str) -> Result<Option<String>, Refusal> {
    let Some(door) = bdq::check_state_verb(args) else { return Ok(None) };
    match vars("SPIRA_BDQ_STATE_WRITE").filter(|v| !v.is_empty()) {
        Some(why) => Ok(Some(format!("{who}: STATE WRITE OVERRIDE ({}) — bd's status moved outside the lifecycle machine: {why}\n", args.join(" ")))),
        None => Err(refused(format!(
            "spira: bdq refuses `{}` — a bead's state is its lifecycle row's, and bd's status follows it, never the reverse.\n\
             Exit: {door}. To write bd's status anyway, set SPIRA_BDQ_STATE_WRITE=<reason> (logged).\n",
            args.first().map(String::as_str).unwrap_or("")
        ))),
    }
}

/// A write every guard let through: what the claim check made of it, and the lines to log.
#[derive(Debug, PartialEq)]
pub struct Admit {
    pub claim: Claim,
    pub log: String,
}

/// Every guard in bdq's order: the create-time fences, the ask-label write fence, the
/// state-verb fence, the czar fence, the claimed-description check. The verdict bdq reaches on
/// this write.
pub fn write(args: &[String], body: Option<&str>, ctx: &Ctx) -> Result<Admit, Refusal> {
    create(args, body, ctx)?;
    ask_label_write(args, ctx)?;
    let log = state_verb(args, ctx.vars, ctx.who)?.unwrap_or_default();
    czar(args, ctx.vars)?;
    Ok(Admit { claim: claim(args, ctx.rows, ctx.now)?, log })
}

/// What a successful write owes the lifecycle machine.
#[derive(Debug, PartialEq, Eq)]
pub enum After {
    Nothing,
    /// A row (`create-bead <id> --ready`) for every bead the write's stdout names.
    Rows,
    /// The closed bead's row made terminal (`reconcile-closed --apply <id>`).
    Terminal(String),
}

/// [`After`] for `args`: a bead-creating verb owes rows unless it files a closed bead or a
/// rowless type; a `close <id>` owes a terminal row.
pub fn after(args: &[String]) -> After {
    if bdq::creates_beads(args) && !(bdq::is_create(args) && (bdq::creates_closed(args) || bdq::creates_rowless_type(args))) {
        return After::Rows;
    }
    match spira_config::lifecycle_row::closed_id(args) {
        Some(id) => After::Terminal(id.to_string()),
        None => After::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn a(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn row(state: &str, holder: &str, lease: Option<i64>) -> Row {
        Row {
            bead_id: "sp-a".into(),
            state: state.into(),
            holder: Some(holder.into()),
            lease_until: lease,
            holds: vec![],
            express: false,
            stack: vec![],
            phase: None,
            disposition: None,
            disposition_note: None,
        }
    }

    const CZAR: &[(&str, &str)] = &[("SPIRA_RUNE", "czar"), ("SPIRA_CZAR_CLASS", "loop-stalled"), ("SPIRA_CZAR_TRIGGER_BEAD", "sp-trig")];

    #[test]
    fn the_stage_answers_with_the_scripts_codes_and_text() {
        let e = env(&[("SPIRA_CZAR_STAGE_LOOP_STALLED", "act"), ("SPIRA_CZAR_STAGE_DEADLOCK", "loud")]);
        let vars = |k: &str| e.get(k).cloned();
        assert_eq!(czar_stage("loop-stalled", &vars), Ok(()));
        assert_eq!(
            czar_stage("attribution-failed", &vars),
            Err(Refusal { code: 1, text: "czar-fence: attribution-failed is shadow — mutation refused (SPIRA_CZAR_STAGE_ATTRIBUTION_FAILED=act to enable)\n".into() })
        );
        assert_eq!(czar_stage("deadlock", &vars), Err(Refusal { code: 2, text: "czar-fence: unknown stage loud in SPIRA_CZAR_STAGE_DEADLOCK (shadow or act)\n".into() }));
        assert_eq!(czar_stage("", &vars), Err(Refusal { code: 2, text: "czar-fence: class required\n".into() }));
    }

    #[test]
    fn the_czar_fence_refuses_a_shadow_class_mutation_with_exit_1() {
        let e = env(CZAR);
        let vars = |k: &str| e.get(k).cloned();
        let shadow = Refusal { code: 1, text: "czar-fence: loop-stalled is shadow — mutation refused (SPIRA_CZAR_STAGE_LOOP_STALLED=act to enable)\n".into() };
        for w in [&["reopen", "sp-trig"][..], &["close", "sp-x"], &["update", "sp-x", "--add-label", "l"]] {
            assert_eq!(czar(&a(w), &vars), Err(shadow.clone()), "{w:?}");
        }
        for w in [&["close", "sp-trig"][..], &["update", "sp-trig", "--set-metadata", "k=v"], &["label", "add", "sp-x", "l"], &["note", "sp-x", "t"], &["create", "T"]] {
            assert_eq!(czar(&a(w), &vars), Ok(()), "{w:?}");
        }
        let unknown = env(&[CZAR[0], CZAR[1], ("SPIRA_CZAR_STAGE_LOOP_STALLED", "loud")]);
        assert_eq!(czar(&a(&["reopen", "sp-x"]), &|k| unknown.get(k).cloned()).map_err(|r| r.code), Err(1), "bdq normalises the script's 2 to 1");
        let acting = env(&[CZAR[0], CZAR[1], ("SPIRA_CZAR_STAGE_LOOP_STALLED", "act")]);
        assert_eq!(czar(&a(&["reopen", "sp-x"]), &|k| acting.get(k).cloned()), Ok(()));
        let builder = env(&[("SPIRA_RUNE", "builder"), CZAR[1]]);
        assert_eq!(czar(&a(&["reopen", "sp-x"]), &|k| builder.get(k).cloned()), Ok(()));
    }

    #[test]
    fn the_claim_check_refuses_a_description_edit_on_a_live_claim_unless_forced() {
        let now = 1_000_000;
        let held = |_: &str| Some(row("WORKING", "dwarf-sp-a", Some(now + 3600)));
        let want = claimdesc::refusal("sp-a", &LiveClaim { assignee: "dwarf-sp-a".into(), lease: claimdesc::iso_of(now + 3600) });
        for w in [&["update", "sp-a", "--title", "T"][..], &["update", "sp-a", "--body-file", "f"], &["update", "sp-a", "-d", "x"]] {
            assert_eq!(claim(&a(w), &held, now), Err(Refusal { code: 1, text: want.clone() }), "{w:?}");
        }
        assert!(matches!(claim(&a(&["update", "sp-a", "--title", "T", "--force-claimed", "why"]), &held, now), Ok(Claim::Forced { reason, .. }) if reason == "why"));
        for w in [&["update", "sp-a", "--set-metadata", "k=v"][..], &["update", "sp-a", "--add-label", "l"], &["close", "sp-a"], &["note", "sp-a", "t"]] {
            assert_eq!(claim(&a(w), &held, now), Ok(Claim::Free), "{w:?}");
        }
        let lapsed = |_: &str| Some(row("WORKING", "dwarf-sp-a", Some(now - 1)));
        assert_eq!(claim(&a(&["update", "sp-a", "--title", "T"]), &lapsed, now), Ok(Claim::Free));
        assert_eq!(claim(&a(&["update", "sp-a", "--title", "T"]), &|_| None, now), Ok(Claim::Free), "an unreadable row refuses nothing");
    }

    #[test]
    fn write_runs_the_czar_fence_before_the_claim_check() {
        let e = env(CZAR);
        let held = |_: &str| Some(row("WORKING", "dwarf-sp-a", None));
        // literal-ok: test fixture label
        let ctx = Ctx { who: "bdq", vars: &|k| e.get(k).cloned(), rows: &held, now: 0, ask_label: &|| Ok("needs-ryan".into()), registry: &registry };
        let r = write(&a(&["update", "sp-a", "--title", "T"]), None, &ctx).unwrap_err();
        assert!(r.text.starts_with("czar-fence: "), "{r:?}");
    }

    fn registry() -> Registry {
        let map = "# name|root|base|gate\nspira|/r/spira|main|gate.sh\ndeck|/r/deck|main|gate.sh\n";
        Registry::new(Some(map), &BTreeMap::new(), std::path::Path::new("/r/spira"))
    }

    fn ctx<'a>(vars: &'a Vars<'a>, ask: &'a dyn Fn() -> Result<String, String>) -> Ctx<'a> {
        Ctx { who: "bdq", vars, rows: &|_| None, now: 0, ask_label: ask, registry: &registry }
    }

    #[test]
    fn the_create_fences_refuse_in_bdqs_order_with_bdqs_text() {
        let none = |_: &str| None;
        // literal-ok: test fixture label
        let ask = || Ok::<_, String>("needs-ryan".into());
        let c = ctx(&none, &ask);
        let refused = |w: &[&str], body: Option<&str>| create(&a(w), body, &c).map_err(|r| (r.code, r.text));
        assert_eq!(refused(&["create", "T", "--labels", "repo:deck"], None), Ok(()));
        let r = refused(&["create", "T", "--labels", "repo:nowhere"], None).unwrap_err();
        assert_eq!((r.0, r.1.starts_with("spira: repo:nowhere is not in the repo map")), (1, true), "{r:?}");
        let r = refused(&["create", "Stop the world", "--description", "run world.sh stop first"], None).unwrap_err();
        // literal-ok: test fixture label
        assert!(r.1.contains("procedures that halt the harness require needs-ryan"), "{r:?}");
        // literal-ok: test fixture label
        let r = refused(&["create", "Pick one", "--labels", "needs-ryan"], None).unwrap_err();
        // literal-ok: test fixture label
        assert!(r.1.contains("needs-ryan"), "an ask without its shape: {r:?}");
        let whole = "## Question\nWhich?\nDefault: a\nClass: policy\n";
        // literal-ok: test fixture label
        assert_eq!(refused(&["create", "Pick one", "--labels", "needs-ryan", "--body-file", "-"], Some(whole)), Ok(()), "the body is the ask's description");
        let r = refused(&["create", "Fix", "--description", "DELETE FROM schema_migrations"], None).unwrap_err();
        assert!(r.1.contains("DELETE FROM schema_migrations"), "{r:?}");
        assert_eq!(refused(&["note", "sp-a", "DELETE FROM schema_migrations"], None), Ok(()), "only a create");
        let down = || Err::<String, String>("SPIRA_ASK_LABEL is not declared".into());
        let c = ctx(&none, &down);
        assert_eq!(create(&a(&["create", "T"]), None, &c), Err(Refusal { code: 1, text: "bdq: SPIRA_ASK_LABEL is not declared\n".into() }));
    }

    #[test]
    fn the_ask_label_is_never_written_onto_an_existing_bead() {
        let none = |_: &str| None;
        // literal-ok: test fixture label
        let ask = || Ok::<_, String>("needs-ryan".into());
        let c = ctx(&none, &ask);
        // literal-ok: test fixture label
        for w in [&["label", "add", "sp-a", "needs-ryan"][..], &["update", "sp-a", "--add-label", "needs-ryan"], &["update", "sp-a", "--set-labels", "x,needs-ryan"]] {
            let r = ask_label_write(&a(w), &c).unwrap_err();
            // literal-ok: test fixture label
            assert!(r.code == 1 && r.text.starts_with("spira: refusing to add needs-ryan to an existing bead"), "{w:?}: {r:?}");
        }
        // literal-ok: test fixture label
        for w in [&["label", "add", "sp-a", "overseer"][..], &["label", "remove", "sp-a", "needs-ryan"], &["update", "sp-a", "--remove-label", "needs-ryan"]] {
            assert_eq!(ask_label_write(&a(w), &c), Ok(()), "{w:?}");
        }
    }

    #[test]
    fn a_state_verb_is_refused_unless_overridden_and_the_override_is_logged() {
        let none = |_: &str| None;
        let r = state_verb(&a(&["close", "sp-a", "--reason", "r"]), &none, "bdq").unwrap_err();
        assert!(r.code == 1 && r.text.starts_with("spira: bdq refuses `close`") && r.text.contains("spira-lc close <id>"), "{r:?}");
        let why = |k: &str| (k == "SPIRA_BDQ_STATE_WRITE").then(|| "landing".to_string());
        assert_eq!(
            state_verb(&a(&["close", "sp-a"]), &why, "bdq"),
            Ok(Some("bdq: STATE WRITE OVERRIDE (close sp-a) — bd's status moved outside the lifecycle machine: landing\n".into()))
        );
        assert_eq!(state_verb(&a(&["update", "sp-a", "--add-label", "x"]), &none, "bdq"), Ok(None));
    }

    #[test]
    fn a_create_owes_rows_and_a_close_a_terminal_row() {
        assert_eq!(after(&a(&["create", "T", "--silent"])), After::Rows);
        assert_eq!(after(&a(&["create", "T", "--type", "event"])), After::Nothing, "a rowless type");
        assert_eq!(after(&a(&["create", "T", "--status", "closed"])), After::Nothing, "filed closed");
        assert_eq!(after(&a(&["close", "sp-a", "--reason", "r"])), After::Terminal("sp-a".into()));
        assert_eq!(after(&a(&["note", "sp-a", "t"])), After::Nothing);
    }
}
