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
        &inputs.halt,
        &inputs.drain,
        inputs.now,
    );
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

    let fixed = head.len() as i64 + tokens.len() as i64 + standing.len() as i64;
    let give = share(rows, fixed, &specs);

    let mut out = Vec::new();
    out.extend(head);
    out.extend(tokens);
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
            renderer_rev: String::new(),
            collector_rev: String::new(),
            tok_win_spark: String::new(),
            now: 1000,
        }
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
}
