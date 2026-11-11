//! The skew guard: a repair sweep acts only while the two planes' planning watermarks agree to
//! within `SPIRA_PLANE_SKEW_MAX`. After a restore the control plane can be seconds old and bd
//! hours old, and then `drop-orphans` would drop READY records bd lacks and the rowless repair
//! would create records for beads the control plane already closed.


use crate::callers::{Bd, Machine};

/// The control-plane key holding `{seq}`, the planning watermark spira-lc stamps on every
/// planning write.
pub const CONTROL_KEY: &str = "planning/watermark";
/// bd's own copy of the same sequence, in its kv, so a restored bd carries the one it was at.
pub const BD_KV_KEY: &str = "spira.planning_watermark";
pub const VERB: &str = "planning-watermark";

/// Whether the planes agree. Neither stamped is a store that has never been restored from
/// a stamp; one stamped and the other not is a skew whose size cannot be told.
pub fn judge(control: Option<u64>, planning: Option<u64>, max: u64) -> Result<(), String> {
    let gap = match (control, planning) {
        (None, None) => return Ok(()),
        (Some(c), Some(p)) => c.abs_diff(p),
        (c, p) => {
            return Err(format!(
                "refused: only one plane carries a planning watermark (control plane {}, bd {}); {}",
                show(c),
                show(p),
                exit()
            ))
        }
    };
    if gap <= max {
        return Ok(());
    }
    Err(format!(
        "refused: the planes' planning watermarks differ by {gap}, past SPIRA_PLANE_SKEW_MAX {max} (control plane {}, bd {}); {}",
        show(control),
        show(planning),
        exit()
    ))
}

fn show(w: Option<u64>) -> String {
    w.map_or("unstamped".into(), |w| w.to_string())
}

/// The way out, named in every refusal (law-a-refusal-names-its-exit).
pub fn exit() -> String {
    format!(
        "the exit is the restore procedure: {}",
        state_store_sqlite::durability::drill::SKEW_PROCEDURE
    )
}

/// bd's kv value of the watermark; `None` while unset.
pub fn parse_kv(value: Option<&str>) -> Result<Option<u64>, String> {
    value.map(|text| text.trim().parse().map_err(|_| format!("{BD_KV_KEY} is {}, not a sequence", text.trim()))).transpose()
}

pub const CHECK_VERB: &str = "skew-check";

/// `skew-check`: exit 0 when the planes agree, 3 with the refusal, 2 when it cannot tell.
/// `control` is `None` when no control store is configured: the control plane is then the
/// lifecycle store a restore does not skew, and there is nothing to guard.
pub fn served(control: Option<Result<Option<u64>, String>>, bd: &mut dyn Bd, max: Result<u64, String>) -> (i32, String) {
    let Some(control) = control else {
        return (0, format!("{CHECK_VERB}: no control store is configured; nothing to guard"));
    };
    let read = || -> Result<(Option<u64>, Option<u64>, u64), String> {
        let max = max?;
        let control = control.map_err(|e| format!("the control plane's planning watermark: {e}"))?;
        let planning = bd.planning_watermark().map_err(|e| format!("bd's planning watermark: {}", e.trim()))?;
        Ok((control, planning, max))
    };
    match read() {
        Err(e) => (crate::CANNOT_TELL, format!("cannot tell: {e}")),
        Ok((c, p, max)) => match judge(c, p, max) {
            Ok(()) => (0, format!("{CHECK_VERB}: the planes agree (control plane {}, bd {})", show(c), show(p))),
            Err(e) => (crate::REFUSED, e),
        },
    }
}

/// What a repair sweep asks before it acts: `Err` carries the exit code and the refusal.
pub fn guard(m: &mut dyn Machine) -> Result<(), (i32, String)> {
    match m.call(&[CHECK_VERB.to_string()]) {
        (0, _) => Ok(()),
        (crate::REFUSED, why) => Err((crate::REFUSED, why)),
        (_, why) => Err((crate::CANNOT_TELL, why)),
    }
}

pub const BOTH_VERB: &str = "plane-watermarks";

/// `plane-watermarks [--manifest]`: `{"control": N|null, "bd": N|null}`, or with `--manifest`
/// the line the brain mirror's manifest carries. A plane that cannot be read cannot tell.
pub fn both(control: Result<Option<u64>, String>, planning: Result<Option<u64>, String>, manifest: bool) -> (i32, String) {
    let (c, p) = match (control, planning) {
        (Ok(c), Ok(p)) => (c, p),
        (Err(e), _) => return (crate::CANNOT_TELL, format!("cannot tell: the control plane's planning watermark: {e}")),
        (_, Err(e)) => return (crate::CANNOT_TELL, format!("cannot tell: bd's planning watermark: {}", e.trim())),
    };
    if manifest {
        return (0, manifest_line(c, p));
    }
    (0, serde_json::json!({ "control": c, "bd": p }).to_string())
}

/// The two watermarks as the brain mirror's manifest records them.
pub fn manifest_line(control: Option<u64>, planning: Option<u64>) -> String {
    format!(
        "Planning watermarks: control plane **{}**, bd **{}**.",
        show(control),
        show(planning)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_planes_agree_within_the_bound_and_are_refused_past_it_naming_the_exit() {
        assert_eq!(judge(None, None, 0), Ok(()));
        assert_eq!(judge(Some(10), Some(7), 3), Ok(()));
        let e = judge(Some(10), Some(6), 3).unwrap_err();
        assert!(e.starts_with("refused: the planes' planning watermarks differ by 4, past SPIRA_PLANE_SKEW_MAX 3 (control plane 10, bd 6)"), "{e}");
        assert!(e.contains("the exit is the restore procedure: after a restore: (0) move SPIRA_RUN/lc-ship aside"), "{e}");
        let one = judge(Some(10), None, 100).unwrap_err();
        assert!(one.contains("only one plane carries a planning watermark (control plane 10, bd unstamped)"), "{one}");
    }

    #[test]
    fn each_plane_s_answer_parses_and_a_garbled_one_cannot_tell() {
        assert_eq!(parse_kv(Some("41\n")), Ok(Some(41)));
        assert_eq!(parse_kv(None), Ok(None));
        assert!(parse_kv(Some("w-7")).is_err());
    }

    struct Kv(Result<Option<u64>, String>);

    impl Bd for Kv {
        fn planning_watermark(&mut self) -> Result<Option<u64>, String> {
            self.0.clone()
        }
        fn issue_type(&mut self, _: &str) -> Result<String, String> {
            unreachable!()
        }
        fn close(&mut self, _: &str, _: &str) -> Result<(), String> {
            unreachable!()
        }
        fn closed(&mut self, _: &[String]) -> Result<Vec<(String, String)>, String> {
            unreachable!()
        }
        fn known(&mut self, _: &[String]) -> Result<Vec<String>, String> {
            unreachable!()
        }
        fn open_among(&mut self, _: &[String]) -> Result<Vec<String>, String> {
            unreachable!()
        }
        fn reopen(&mut self, _: &str) -> Result<(), String> {
            unreachable!()
        }
        fn work_beads(&mut self, _: &str) -> Result<Vec<String>, String> {
            unreachable!()
        }
        fn external(&mut self, _: &str) -> Result<bool, String> {
            unreachable!()
        }
        fn blocking_dependents(&mut self, _: &str) -> Result<Vec<String>, String> {
            unreachable!()
        }
    }

    #[test]
    fn skew_check_refuses_past_the_bound_cannot_tell_on_a_failed_read_and_passes_without_a_control_store() {
        let (rc, out) = served(Some(Ok(Some(90))), &mut Kv(Ok(Some(40))), Ok(25));
        assert_eq!(rc, crate::REFUSED, "{out}");
        assert!(out.contains("differ by 50, past SPIRA_PLANE_SKEW_MAX 25") && out.contains("the exit is the restore procedure"), "{out}");
        assert_eq!(served(Some(Ok(Some(90))), &mut Kv(Ok(Some(80))), Ok(25)).0, 0);
        assert_eq!(served(Some(Ok(Some(90))), &mut Kv(Err("bd down".into())), Ok(25)).0, crate::CANNOT_TELL);
        assert_eq!(served(Some(Err("locked".into())), &mut Kv(Ok(None)), Ok(25)).0, crate::CANNOT_TELL);
        assert_eq!(served(Some(Ok(None)), &mut Kv(Ok(None)), Err("unset".into())).0, crate::CANNOT_TELL);
        assert_eq!(served(None, &mut Kv(Ok(Some(1))), Ok(0)).0, 0);
    }

    #[test]
    fn the_manifest_line_names_both_planes() {
        assert_eq!(manifest_line(Some(5), None), "Planning watermarks: control plane **5**, bd **unstamped**.");
        assert_eq!(both(Ok(Some(5)), Ok(Some(4)), true), (0, "Planning watermarks: control plane **5**, bd **4**.".into()));
        assert_eq!(both(Ok(None), Ok(Some(4)), false), (0, r#"{"control":null,"bd":4}"#.into()));
        assert_eq!(both(Ok(None), Err("down".into()), true).0, crate::CANNOT_TELL);
    }
}
