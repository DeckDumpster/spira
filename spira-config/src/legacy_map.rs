//! The legacy repo-map's land/base columns — the one writer of them outside a hand edit,
//! kept here because spira-config is the only door onto either config file
//! (law-config-through-the-cli-only). It exists for one caller, queue's land-mode transition
//! (to-forge/to-local), which must keep repo-map in step with spira.toml for as long as bash
//! readers (lib.sh repo_field) still read repo-map; it retires with the config epic that
//! deletes repo-map. A pure text transform plus an atomic write: only columns 3 (land) and 4
//! (base) of the matching row change, every other byte survives — identical to queue.sh's
//! own awk (_land_mode_write_row).

#[derive(Debug, PartialEq, Eq)]
pub enum MapError {
    /// The row has fewer than six fields: it predates the base column.
    ShortRow,
    /// No row names the repository.
    NoRow,
    /// The rewrite came out empty.
    Empty,
}

pub fn rewrite_row(text: &str, name: &str, land: &str, base: &str) -> Result<String, MapError> {
    let mut out = String::with_capacity(text.len());
    let mut hit = false;
    for line in text.split_inclusive('\n') {
        let (body, nl) = match line.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (line, ""),
        };
        if body.trim_start().starts_with('#') {
            out.push_str(line);
            continue;
        }
        let mut fields: Vec<&str> = body.split('|').collect();
        if fields[0].trim() == name {
            if fields.len() < 6 {
                return Err(MapError::ShortRow);
            }
            let l = format!(" {land} ");
            let b = format!(" {base} ");
            fields[2] = &l;
            fields[3] = &b;
            out.push_str(&fields.join("|"));
            out.push_str(nl);
            hit = true;
            continue;
        }
        out.push_str(line);
    }
    if !hit {
        return Err(MapError::NoRow);
    }
    if out.is_empty() {
        return Err(MapError::Empty);
    }
    Ok(out)
}

/// Rewrite `name`'s land/base in the repo-map file at `path`, atomically.
pub fn set_row_in_file(path: &std::path::Path, name: &str, land: &str, base: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let new = rewrite_row(&text, name, land, base).map_err(|e| match e {
        MapError::ShortRow => format!("{name} row in {} has no base column (NF<6) — add one by hand first", path.display()),
        MapError::NoRow => format!("{} has no {name} row", path.display()),
        MapError::Empty => "the rewrite produced an empty file".to_string(),
    })?;
    crate::write_atomic(path, &new).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_land_and_base_change() {
        let t = "# name | path | land | base | format | gate\nspira | /h | queue.local | local/main | | bash a | b && c\nother | /o | push | origin/main | | x\n";
        let r = rewrite_row(t, "spira", "queue.forge", "origin/main").unwrap();
        assert_eq!(
            r,
            "# name | path | land | base | format | gate\nspira | /h | queue.forge | origin/main | | bash a | b && c\nother | /o | push | origin/main | | x\n"
        );
    }

    #[test]
    fn short_row_and_missing_row_refuse() {
        assert_eq!(rewrite_row("spira | /h | queue | x | y\n", "spira", "queue.local", "local/main"), Err(MapError::ShortRow));
        assert_eq!(rewrite_row("a | /h | queue | x | y | z\n", "spira", "queue.local", "local/main"), Err(MapError::NoRow));
    }
}
