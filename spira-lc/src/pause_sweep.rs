//! `pause-sweep`: lifts every scoped stop whose `bead:ID=STATE` release condition has been met,
//! judged on the bead's applied lifecycle events since the stop was set, so a bead that entered
//! the state and moved on between two sweeps still releases it.

use crate::db::Conn;
use crate::CANNOT_TELL;
use spira_config::scoped_stop;

pub fn run(conn: &Conn) -> (i32, String) {
    let run = match crate::lifecycle_run_dir() {
        Ok(d) => d,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    let mut reached = |bead: &str, state: &str, since: u64| {
        let sql = format!(
            "SELECT to_state, applied, `at` FROM event WHERE machine = 'bead' AND lc_key = '{}' AND to_state = '{}' AND applied = 1 AND `at` >= {since}",
            crate::rows::escape(bead),
            crate::rows::escape(state)
        );
        conn.query(&sql).map(|rows| scoped_stop::entered_state(&rows, state, since)).map_err(|e| format!("cannot tell: {e:?}"))
    };
    let (lifted, unread) = scoped_stop::sweep(&run, &mut reached);
    let mut lines: Vec<String> = lifted.iter().map(|r| format!("pause-sweep: lifted the stop on {r}: its bead condition was met")).collect();
    lines.extend(unread.iter().map(|u| format!("pause-sweep: kept {u}")));
    (if unread.is_empty() { 0 } else { CANNOT_TELL }, lines.join("\n"))
}
