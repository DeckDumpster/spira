//! Every effect the intake has outside its own arguments, as a trait (DESIGN.md §5):
//! `bd`, `mail`, the repository map and the clock. `real.rs` implements them against the
//! host; unit tests implement them as fakes so the orchestration in `run.rs` is testable
//! without a database.

use crate::decide::BeadRow;

pub trait Bd {
    /// `bd -C db list --status <which> [--label <label>] [--closed-after <date>]
    /// --limit 0 --json`, parsed into rows. Err when bd could not be reached at all
    /// (distinct from an empty Ok(vec![]), which is a real "nothing found"). The rows are
    /// incident records, a non-work kind whose bd status is its state
    /// (`spira_config::nonwork::Kind::Incident`, sp-mve9i).
    fn list(
        &self,
        db: &str,
        which: spira_config::nonwork::Which,
        label: Option<&str>,
        closed_after: Option<&str>,
    ) -> Result<Vec<BeadRow>, String>;

    /// `bd -C db create <title> --type T --priority P --labels L --external-ref R
    /// --body-file -` (payload on stdin) -> the new bead id.
    #[allow(clippy::too_many_arguments)]
    fn create(
        &self,
        db: &str,
        title: &str,
        kind: &str,
        priority: &str,
        labels: &str,
        external_ref: &str,
        body: &str,
        actor: Option<&str>,
    ) -> Result<String, String>;

    fn label_add(&self, db: &str, id: &str, label: &str) -> bool;
    fn label_remove(&self, db: &str, id: &str, label: &str) -> bool;
    fn label_list(&self, db: &str, id: &str) -> Vec<String>;
    fn note(&self, db: &str, id: &str, text: &str) -> bool;
    fn set_state(&self, db: &str, id: &str, kv: &str) -> bool;
    /// `bd -C db reopen <id>`.
    fn reopen(&self, db: &str, id: &str) -> bool;
    fn show_closed_at(&self, db: &str, id: &str) -> Option<String>;
    fn show_created_at(&self, db: &str, id: &str) -> Option<String>;
    /// A cheap reachability probe (`bd list --limit 1`) — used to distinguish "no open
    /// incident" from "the database could not be reached" when the dedup scan is empty.
    fn reachable(&self, db: &str) -> bool;
    /// Raw `bd -C db sql "<query>"`; Err when bd itself could not be reached/run.
    fn sql(&self, db: &str, query: &str) -> Result<String, String>;
}

/// The events-table counters (`bump_recur`/`recurs_of`, lib.sh) built on top of `Bd::sql`.
pub fn bump_recur(bd: &dyn Bd, db: &str, id: &str, cause: &str) {
    let uuid = uuid_v4();
    let q = format!(
        "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('{uuid}', '{id}', 'recurred', 'incident', '{cause}', UTC_TIMESTAMP())"
    );
    let _ = bd.sql(db, &q);
}

/// `None` means the query failed and the count is genuinely unknown (blind), never 0 —
/// law-absence-needs-a-positive-control (sp-39yd3).
pub fn recurs_of(bd: &dyn Bd, db: &str, id: &str) -> Option<u32> {
    let q = format!("SELECT COUNT(*) FROM events WHERE issue_id='{id}' AND event_type='recurred'");
    let out = bd.sql(db, &q).ok()?;
    // bd's tabular output for a single-column SELECT: a header line, a separator line,
    // then the value (incident-stub-bd.py mirrors this exactly: "header" / "----" / N).
    let line = out.lines().nth(2)?;
    line.trim().parse::<u32>().ok()
}

fn uuid_v4() -> String {
    // A dependency-free v4 UUID: 122 random bits from the OS CSPRNG via a /dev/urandom
    // read, formatted per RFC 4122. Only used as an events-row primary key, never compared
    // or parsed back, so format fidelity (not a crypto property) is what matters.
    let mut bytes = [0u8; 16];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .is_err()
    {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = ((nanos >> (i * 8)) & 0xff) as u8;
        }
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}{}{}{}-{}{}-{}{}-{}{}-{}{}{}{}{}{}",
        hex[0], hex[1], hex[2], hex[3], hex[4], hex[5], hex[6], hex[7], hex[8], hex[9], hex[10], hex[11], hex[12],
        hex[13], hex[14], hex[15]
    )
}

pub trait Mailer {
    /// `mail send operator --from "Incident <incident@spira>" --subject S --kind
    /// question --default D`, body on stdin. True on success.
    fn send_operator_question(&self, subject: &str, default: &str, body: &str) -> bool;
}

pub trait Clock {
    fn now(&self) -> i64;
}
