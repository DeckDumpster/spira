//! `frame`/`render`/`paint` — assemble one frame for a pane of a given size, cut it to what
//! the pane can actually show, and paint it without flicker.
//!
//! The elastic sections are rendered IN FULL first, then cut to their `share`d allocation —
//! building them first is what makes the allocator honest: it is told what each section
//! could actually use, rather than a guess made before the data was read.
//!
//! INDICES ARE POSITIONAL: NOW=0, NEXT=1, UNLANDED=2, RECENT=3, INFLOW=4, CI=5. Adding a
//! section in the middle shifts every index after it in `frame`'s `spec` array.

use super::model::Snapshot;
use super::sections::*;
use super::share::{share, Spec};

pub struct FrameInputs<'a> {
    pub snapshot_content: &'a str,
    pub snapshot_exists: bool,
    pub cols: i64,
    pub hhmm: String,
    pub age_secs: Option<i64>,
    pub snap_stale_s: i64,
    pub live_aeon_n: i64,
    pub trace_lines: i64,
    pub halt: HaltState,
    pub drain: DrainState,
    pub run_dir: String,
    pub mismatch_alt: Option<String>,
    pub renderer_rev: String,
    pub collector_rev: String,
    pub tok_win_spark: String,
    pub now: i64,
}

pub fn frame(rows: i64, cols_in: i64, inputs: &FrameInputs) -> Vec<String> {
    let cols = if cols_in > 0 { cols_in } else { 80 };
    let snap = Snapshot::parse(inputs.snapshot_content);
    let stalled = Snapshot::stalled_probes(inputs.snapshot_content);

    let head = header_line(
        &snap,
        cols,
        &inputs.hhmm,
        inputs.age_secs,
        inputs.snap_stale_s,
        &stalled,
        inputs.snapshot_exists,
        &inputs.run_dir,
        inputs.mismatch_alt.as_deref(),
        &inputs.halt,
        &inputs.drain,
        inputs.now,
    );
    let slots = slots_line(&snap);
    let tokens = tokens_section(
        &snap,
        cols,
        &TokensExtra {
            renderer_rev: &inputs.renderer_rev,
            collector_rev: &inputs.collector_rev,
            tok_win_spark: &inputs.tok_win_spark,
        },
    );
    let now_v = now_section(&snap, cols, inputs.live_aeon_n, inputs.trace_lines);
    let next_v = next_section(&snap, cols);
    let unlanded = queue_section(&snap, cols);
    let recent = recent_section(&snap, cols, inputs.snapshot_exists);
    let inflow = inflow_section(&snap, cols);
    let ci = ci_section(&snap, cols);
    let standing = standing_lines(&snap, cols);
    let round = super::round::round_section(&snap, cols, inputs.now, rows);
    let flow = flow_lines(&snap);

    let want = [
        now_v.len() as i64,
        next_v.len() as i64,
        (unlanded.len() as i64).min(MAX_SECTION_ROWS),
        recent.len() as i64,
        inflow.len() as i64,
        (ci.len() as i64).min(MAX_SECTION_ROWS),
    ];

    let nb = (NEXT_BASE_ROWS + 1).min(want[1]);
    let rb = RECENT_BASE_ROWS.min(want[3]);
    let ib = (INFLOW_BASE_ROWS + 1).min(want[4]);

    let specs = [
        Spec::new(want[0], want[0], true),
        Spec::new(nb, want[1], false),
        Spec::new(want[2], want[2], true),
        Spec::new(rb, want[3], false),
        Spec::new(ib, want[4], false),
        Spec::new(want[5], want[5], true),
    ];

    let fixed = head.len() as i64 + 1 + tokens.len() as i64 + round.len() as i64 + flow.len() as i64 + standing.len() as i64;
    let give = share(rows, fixed, &specs);

    let mut out = Vec::new();
    out.extend(head);
    out.push(slots);
    out.extend(tokens);
    out.extend(round);
    out.extend(flow);
    out.extend(now_v.into_iter().take(give[0].max(0) as usize));
    out.extend(next_v.into_iter().take(give[1].max(0) as usize));
    out.extend(unlanded.into_iter().take(give[2].max(0) as usize));
    out.extend(recent.into_iter().take(give[3].max(0) as usize));
    out.extend(inflow.into_iter().take(give[4].max(0) as usize));
    out.extend(ci.into_iter().take(give[5].max(0) as usize));
    out.extend(standing);
    out
}

/// `render <rows> <cols>` — `frame`, cut to what the pane can actually show, with the drop
/// count marked on the header line rather than silently scrolling the top away.
pub fn render(rows: i64, cols: i64, inputs: &FrameInputs) -> Vec<String> {
    let mut all = frame(rows, cols, inputs);
    let n = all.len() as i64;
    if rows > 0 && n > rows {
        let drop = n - rows;
        if let Some(first) = all.first_mut() {
            first.push_str(&format!("  {}\u{25be}{drop}{}", super::colors::DIM, super::colors::RST));
        }
        all.truncate(rows as usize);
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_inputs() -> FrameInputs<'static> {
        FrameInputs {
            snapshot_content: "",
            snapshot_exists: false,
            cols: 80,
            hhmm: "12:00".to_string(),
            age_secs: None,
            snap_stale_s: 60,
            live_aeon_n: 0,
            trace_lines: 2,
            halt: HaltState { stamp_exists: false, since: String::new(), why: String::new(), sentinel_active: Some(true) },
            drain: DrainState { stamp_mtime: None },
            run_dir: "/run".to_string(),
            mismatch_alt: None,
            renderer_rev: String::new(),
            collector_rev: String::new(),
            tok_win_spark: String::new(),
            now: 1000,
        }
    }

    fn strip(s: &str) -> String {
        let mut o = String::new();
        let mut esc = false;
        for c in s.chars() {
            if esc { if c.is_ascii_alphabetic() { esc = false } } else if c == '\x1b' { esc = true } else { o.push(c) }
        }
        o
    }

    fn slots_frame(snap: &'static str, rows: i64) -> String {
        let mut i = base_inputs();
        i.snapshot_content = snap;
        i.snapshot_exists = true;
        render(rows, 80, &i).iter().map(|l| strip(l)).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn slots_row_shows_live_ceiling_free_and_split() {
        let out = slots_frame("SP_SLOTS_LIVE='3'\nSP_SLOTS_CEILING='8'\nSP_SLOTS_FREE='5'\nSP_SLOTS_POOL='8'\nSP_SLOTS_LANES_CAP='2'\n", 0);
        assert!(out.contains("SLOTS   3/8  5 free  (builders 6-8, lanes 0-2)"), "{out}");
    }

    #[test]
    fn slots_row_has_no_split_without_a_lane_cap_and_shows_question_on_failed_probe() {
        let out = slots_frame("SP_SLOTS_LIVE='1'\nSP_SLOTS_CEILING='4'\nSP_SLOTS_FREE='3'\nSP_SLOTS_POOL='4'\nSP_SLOTS_LANES_CAP=''\n", 0);
        assert!(out.contains("1/4  3 free") && !out.contains("builders"), "{out}");
        let bad = slots_frame("SP_SLOTS_LIVE='?'\nSP_SLOTS_CEILING='?'\nSP_SLOTS_FREE='?'\n", 0);
        assert!(bad.contains("? free"), "{bad}");
    }

    #[test]
    fn slots_row_survives_a_mail_section_longer_than_the_pane() {
        let mut snap = String::from("SP_SLOTS_LIVE='3'\nSP_SLOTS_CEILING='8'\nSP_SLOTS_FREE='5'\nSP_MAIL_UNREAD='5'\nSP_MAIL_OLDEST_AGE='120'\nSP_MAIL_N='5'\n");
        for n in 0..5 { snap.push_str(&format!("SP_MAIL{n}='60|NEW|a mail message long enough to matter {n}'\n")); }
        let out = slots_frame(Box::leak(snap.into_boxed_str()), 8);
        assert!(out.contains("5 free"), "{out}");
    }

    #[test]
    fn the_header_faults_above_a_non_default_threshold_and_not_at_it() {
        let mut stale = base_inputs();
        stale.snap_stale_s = 7;
        stale.age_secs = Some(20);
        assert!(frame(0, 80, &stale)[0].contains("FAULT (20s)"));
        let mut edge = base_inputs();
        edge.snap_stale_s = 7;
        edge.age_secs = Some(7);
        assert!(!frame(0, 80, &edge)[0].contains("FAULT"));
        let mut fresh = base_inputs();
        fresh.snap_stale_s = 7;
        fresh.age_secs = Some(0);
        assert!(!frame(0, 80, &fresh)[0].contains("FAULT"));
    }

    #[test]
    fn frame_with_no_snapshot_still_produces_a_header_and_standing_lines() {
        let out = frame(0, 80, &base_inputs());
        assert!(!out.is_empty());
    }

    #[test]
    fn render_marks_drop_count_on_the_header_when_pane_is_too_short() {
        let out = render(3, 80, &base_inputs());
        assert_eq!(out.len(), 3);
        assert!(out[0].contains('\u{25be}'));
    }

    #[test]
    fn render_with_zero_rows_means_unlimited() {
        let unlimited = render(0, 80, &base_inputs());
        let bounded = frame(0, 80, &base_inputs());
        assert_eq!(unlimited.len(), bounded.len());
    }

    /// The regression this pins: `term_size` falling to a small hardcoded default (the
    /// `stty size`-via-a-null-stdin bug, sp-llbmi) made `render` believe the pane was 5
    /// rows tall no matter what it actually was, so on the operator's real 81-row pane
    /// only the header and a fold marker ever showed. This asserts the OTHER half of the
    /// fix holds: given the pane's TRUE height (81, the exact live-pane reproduction), every
    /// major body section is present, not just the header — i.e. a correctly-read 81 must
    /// actually render 81 rows' worth of content, never fold down to a handful regardless
    /// of `term_size`'s own correctness.
    #[test]
    fn a_correctly_read_81_row_pane_renders_every_body_section() {
        let mut inputs = base_inputs();
        inputs.cols = 168;
        // A non-trivial snapshot, so NOW/NEXT/QUEUE/RECENT/INFLOW/CI have real content to
        // show rather than their single-line "unread"/"empty" shapes — the fold bug is
        // about budget, and a budget bug can hide behind sections that were only ever
        // going to be one line regardless.
        inputs.snapshot_content = "SP_AT='1000000000'\n\
             SP_SENTINEL_TIMER='1'\nSP_OPS_TIMER='1'\nSP_AURON_TIMER='1'\n\
             SP_AEON_N='1'\nSP_AEON0_NAME='shiva'\nSP_AEON0_FAYTH='builder'\n\
             SP_AEON0_BEAD='sp-llbmi'\nSP_AEON0_MIN='1'\nSP_AEON0_TURNS='1'\n\
             SP_AEON0_CTX='100'\nSP_AEON0_MODEL='claude-sonnet-4-6'\n\
             SP_AEON0_FAYTH_MODEL='claude-sonnet-4-6'\nSP_AEON0_FILES='1'\n\
             SP_AEON0_PRI='1'\nSP_AEON0_PARTITION='builder'\nSP_AEON0_TITLE='t'\n\
             SP_AEON0_LEASE='8'\nSP_AEON0_QUIET='1'\n\
             SP_NEXT_N='1'\nSP_NEXT0='P1 builder sp-a some next bead'\n\
             SP_EVENT0='5s builder landed sp-b a recent landing'\n\
             SP_INFLOW_N='1'\nSP_INFLOW0='5s task P1 sp-c an inflow bead'\n\
             SP_AWAITING_N='1'\nSP_AWAITING0='sp-d waiting on ci'\n";
        inputs.snapshot_exists = true;
        let out = render(81, 168, &inputs);
        let joined = out.join("\n");
        for label in ["NOW", "NEXT", "QUEUE", "RECENT", "INFLOW", "CI", "ATTN", "SEND", "BEADS", "LAND"] {
            assert!(
                joined.contains(label),
                "expected the {label} section in an 81-row render, got:\n{joined}"
            );
        }
        // And it must not have folded: 81 rows asked for, 81 rows (or fewer, if the frame
        // itself is genuinely shorter) delivered — never truncated with a drop marker.
        assert!(out.len() <= 81);
        assert!(!out[0].contains('\u{25be}'), "header carries a fold-drop marker: {}", out[0]);
    }
}
