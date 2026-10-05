// Planted (sp-mve9i): every one of these decides on a work bead's bd status or assignee.
pub fn has_open_child(list: &[Bead], id: &str) -> bool {
    list.iter().any(|b| b.parent.as_deref() == Some(id) && b.status != "closed")
}

pub fn remedies(bd: &Bd) -> String {
    bd.run(&["list", "--status", "closed", "--label-pattern", "covers:*", "--json"])
}

pub fn outcome(status: &str) -> &'static str {
    match status {
        "in_progress" => "running",
        _ => "idle",
    }
}

pub fn claimed(b: &Bead) -> bool {
    if b.assignee.is_some() { true } else { false }
}
