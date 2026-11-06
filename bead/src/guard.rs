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
//! Both refuse with exit 1 and the text bdq printed. Each reads the caller's own context —
//! its environment ([`Vars`]) and the lifecycle row of the bead it names — through a port,
//! so the guard logic is one function whichever process holds the context.

use crate::claimdesc::{self, LiveClaim};
use spira_config::lc_state::Row;

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

/// Both guards in bdq's order (the czar fence first): the verdict bdq reaches on this write.
pub fn write(args: &[String], vars: &Vars, rows: &Rows, now: i64) -> Result<Claim, Refusal> {
    czar(args, vars)?;
    claim(args, rows, now)
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
        let r = write(&a(&["update", "sp-a", "--title", "T"]), &|k| e.get(k).cloned(), &held, 0).unwrap_err();
        assert!(r.text.starts_with("czar-fence: "), "{r:?}");
    }
}
