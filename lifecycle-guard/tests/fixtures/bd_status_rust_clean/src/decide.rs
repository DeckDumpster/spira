// The same decisions, through the machine and the non-work scope.
use spira_config::{lc_state, nonwork};

pub fn has_open_child(list: &[Bead], rows: &Rows, id: &str) -> bool {
    list.iter().any(|b| b.parent.as_deref() == Some(id) && rows.get(&b.id).is_some_and(|r| !r.terminal()))
}

pub fn open_asks(bd: &Bd, label: &str) -> String {
    let [flag, value] = nonwork::status_args(nonwork::Kind::Ask, nonwork::Which::Open);
    bd.run(&["list", &flag, &value, "--label", label, "--json"])
}

pub fn finished(o: &std::process::Output, pr_state: &str) -> bool {
    // `b.status == "closed"` was the old check; a PR's own state is GitHub's.
    o.status.success() && pr_state == "closed"
}

#[cfg(test)]
mod tests {
    #[test]
    fn fixture_rows_may_name_bd_status() {
        assert!(super::row().status == "closed");
    }
}
