//! Reads `SPIRA_WATCHERS` and its overlay directory off disk and parses them into one
//! manifest (`watchd_rows`). The only place in this crate that touches the manifest files
//! themselves; `manifest::parse_file` does the parsing and is tested without a filesystem.

use crate::manifest::{self, Resolver, Row};
use std::path::Path;

pub enum RowsError {
    /// `SPIRA_WATCHERS` itself does not exist — distinct from a malformed one: there is
    /// nothing to be malformed.
    Missing(String),
    /// At least one line, in the harness file or an overlay file, did not parse. Every
    /// fault found, across every file — never just the first — because the caller's
    /// refusal should let an operator fix everything in one pass.
    Malformed(Vec<String>),
}

pub fn load(watchers: &Path, overlay_dir: &Path, resolver: &dyn Resolver) -> Result<Vec<Row>, RowsError> {
    if !watchers.is_file() {
        return Err(RowsError::Missing(format!("no watcher manifest at {}", watchers.display())));
    }
    let mut files = vec![watchers.to_path_buf()];
    if overlay_dir.is_dir() {
        let mut overlays: Vec<_> = std::fs::read_dir(overlay_dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().map(|e| e == "watchers").unwrap_or(false))
            .collect();
        overlays.sort();
        files.extend(overlays);
    }

    let mut rows = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut faults = Vec::new();
    let harness = std::fs::read_to_string(watchers).unwrap_or_default();
    manifest::parse_file(&watchers.display().to_string(), &harness, resolver, &mut rows, &mut seen, &mut faults);
    let shipped: std::collections::HashSet<String> = seen.clone();
    let mut overlay_seen = std::collections::HashSet::new();
    for file in &files[1..] {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        let label = file.display().to_string();
        let mut mine = Vec::new();
        manifest::parse_file(&label, &text, resolver, &mut mine, &mut overlay_seen, &mut faults);
        for row in mine {
            if shipped.contains(&row.name) {
                eprintln!("watchd: {label}: '{}' is now shipped by the harness manifest; the manifest's row stands and this one is ignored — remove it from the overlay", row.name);
            } else {
                rows.push(row);
            }
        }
    }
    if !faults.is_empty() {
        return Err(RowsError::Malformed(faults));
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::MapResolver;
    use std::collections::HashMap;
    use testkit::TempDir;

    fn resolver() -> MapResolver<'static> {
        MapResolver(Box::leak(Box::new(HashMap::new())))
    }

    #[test]
    fn a_missing_manifest_is_reported_as_missing_not_malformed() {
        let d = TempDir::new("watchd-rows");
        let r = load(&d.join("nope"), &d.join("overlay"), &resolver());
        assert!(matches!(r, Err(RowsError::Missing(_))));
    }

    #[test]
    fn a_good_manifest_with_no_overlay_dir_parses() {
        let d = TempDir::new("watchd-rows");
        std::fs::write(d.join("watchers"), "pool|daemon|pool.sh\n").unwrap();
        let rows = load(&d.join("watchers"), &d.join("no-such-overlay"), &resolver()).ok().unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn overlay_watchers_files_are_merged_in_sorted_order_and_share_the_name_space() {
        let d = TempDir::new("watchd-rows");
        std::fs::write(d.join("watchers"), "pool|daemon|pool.sh\n").unwrap();
        std::fs::create_dir(d.join("overlay")).unwrap();
        std::fs::write(d.join("overlay/b.watchers"), "mine|daemon|mine.sh\n").unwrap();
        std::fs::write(d.join("overlay/a.watchers"), "other|daemon|other.sh\n").unwrap();
        std::fs::write(d.join("overlay/not-a-manifest.txt"), "pool|daemon|x.sh\n").unwrap();
        let rows = load(&d.join("watchers"), &d.join("overlay"), &resolver()).ok().unwrap();
        assert_eq!(rows.len(), 3, "the .txt file must not be picked up");
        assert_eq!(rows[0].name, "pool");
        assert_eq!(rows[1].name, "other", "a.watchers sorts before b.watchers");
        assert_eq!(rows[2].name, "mine");
    }

    #[test]
    fn an_overlay_row_the_harness_manifest_now_ships_is_skipped_and_the_manifest_row_stands() {
        let d = TempDir::new("watchd-rows");
        std::fs::write(d.join("watchers"), "pool|daemon|pool.sh\n").unwrap();
        std::fs::create_dir(d.join("overlay")).unwrap();
        std::fs::write(d.join("overlay/x.watchers"), "pool|daemon|other.sh\nmine|daemon|mine.sh\n").unwrap();
        let rows = load(&d.join("watchers"), &d.join("overlay"), &resolver()).ok().unwrap();
        let names: Vec<(&str, &str)> = rows.iter().map(|r| (r.name.as_str(), r.target.as_str())).collect();
        assert_eq!(names, vec![("pool", "pool.sh"), ("mine", "mine.sh")]);
    }

    #[test]
    fn a_duplicate_name_between_two_overlay_files_is_still_a_fault() {
        let d = TempDir::new("watchd-rows");
        std::fs::write(d.join("watchers"), "pool|daemon|pool.sh\n").unwrap();
        std::fs::create_dir(d.join("overlay")).unwrap();
        std::fs::write(d.join("overlay/a.watchers"), "mine|daemon|a.sh\n").unwrap();
        std::fs::write(d.join("overlay/b.watchers"), "mine|daemon|b.sh\n").unwrap();
        let r = load(&d.join("watchers"), &d.join("overlay"), &resolver());
        assert!(matches!(r, Err(RowsError::Malformed(_))));
    }

    #[test]
    fn one_bad_line_in_an_overlay_file_refuses_the_whole_manifest() {
        let d = TempDir::new("watchd-rows");
        std::fs::write(d.join("watchers"), "pool|daemon|pool.sh\n").unwrap();
        std::fs::create_dir(d.join("overlay")).unwrap();
        std::fs::write(d.join("overlay/x.watchers"), "garbage\n").unwrap();
        let r = load(&d.join("watchers"), &d.join("overlay"), &resolver());
        match r {
            Err(RowsError::Malformed(faults)) => assert_eq!(faults.len(), 1),
            _ => panic!("expected Malformed"),
        }
    }
}
