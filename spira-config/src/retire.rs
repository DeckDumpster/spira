//! `spira-config retire`: the changes the rules require when a config key or a scheduler task
//! goes away. The protocol is written once, in `release/DESIGN.md` ("Retiring a key or a task").

use std::fs;
use std::path::Path;

pub const DELTA: &str = "spira/config-delta.toml";
pub const FIXTURE: &str = "spira/testlib/complete.toml";
const REGISTRY_DIRS: [&str; 2] = ["spira/conf.d", "spira/conf.toml.d"];
const TASK_MODES: &str = "spira/conf.d/SPIRA_TASK_MODES";

/// What a retirement changed, and what it leaves to the builder.
#[derive(Debug, Default)]
pub struct Report {
    pub changed: Vec<String>,
    pub left: Vec<String>,
}

impl Report {
    pub fn render(&self) -> String {
        let mut out = String::new();
        for c in &self.changed {
            out.push_str(&format!("changed: {c}\n"));
        }
        for l in &self.left {
            out.push_str(&format!("left to you: {l}\n"));
        }
        out
    }
}

fn read(root: &Path, rel: &str) -> Result<String, String> {
    fs::read_to_string(root.join(rel)).map_err(|e| format!("{rel}: {e}"))
}

fn write(root: &Path, rel: &str, text: &str) -> Result<(), String> {
    fs::write(root.join(rel), text).map_err(|e| format!("{rel}: {e}"))
}

fn fragment_path(bead: &str) -> Result<String, String> {
    if bead.is_empty() || bead.starts_with('.') || !bead.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        return Err(format!("{bead:?} is not a bead id"));
    }
    Ok(format!("spira/config-delta.d/{bead}.toml"))
}

/// Adds `entry` to `list` in the bead's fragment, refusing one the merged delta already lists.
fn fragment_list(root: &Path, bead: &str, list: &str, entry: &str) -> Result<(String, String), String> {
    let frag = fragment_path(bead)?;
    let merged = crate::delta::delta_text(&root.join("spira"))?.unwrap_or_default();
    let merged: toml::Value = merged.parse().map_err(|e: toml::de::Error| format!("{DELTA}: {e}"))?;
    if merged.get(list).and_then(|v| v.as_array()).is_some_and(|a| a.iter().any(|v| v.as_str() == Some(entry))) {
        return Err(format!("the config delta already lists {entry} under {list}"));
    }
    let text = fs::read_to_string(root.join(&frag)).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| format!("{frag}: {e}"))?;
    let item = doc.entry(list).or_insert(toml_edit::value(toml_edit::Array::new()));
    item.as_array_mut().ok_or_else(|| format!("{frag}: {list} is not a list"))?.push(entry);
    Ok((frag, doc.to_string()))
}

fn write_fragment(root: &Path, frag: &str, text: &str) -> Result<(), String> {
    fs::create_dir_all(root.join("spira/config-delta.d")).map_err(|e| format!("spira/config-delta.d: {e}"))?;
    write(root, frag, text)
}

/// Retires `spira.<field>`: deletes its registry file and names it in the delta's `removed`.
pub fn retire_key(root: &Path, dotted: &str, bead: &str) -> Result<Report, String> {
    let field = dotted.strip_prefix("spira.").filter(|f| !f.is_empty() && !f.contains('.')).ok_or_else(|| format!("{dotted:?} is not a spira.<key> path"))?;
    let file = REGISTRY_DIRS
        .iter()
        .find_map(|d| {
            fs::read_dir(root.join(d)).ok()?.flatten().find_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                (crate::registry::field_of(&n) == field && n.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')).then(|| format!("{d}/{n}"))
            })
        })
        .ok_or_else(|| format!("no registry file declares {dotted} under {}", REGISTRY_DIRS.join(" or ")))?;
    let (frag, text) = fragment_list(root, bead, "removed", dotted)?;
    fs::remove_file(root.join(&file)).map_err(|e| format!("{file}: {e}"))?;
    write_fragment(root, &frag, &text)?;

    let mut rep = Report::default();
    rep.changed.push(format!("removed {file}"));
    rep.changed.push(format!("{frag}: {dotted} added to removed"));
    if read(root, FIXTURE).is_ok_and(|f| f.lines().any(|l| l.starts_with(&format!("{field} =")))) {
        rep.left.push(format!(
            "the `{field}` line stays in {FIXTURE} while the active release registers the key (every key is required, and `work submit`'s reads gate refuses an early drop): file nothing for it; the first later branch that edits {FIXTURE} after the key has left the active release deletes the line as a small fix-up"
        ));
    }
    rep.left.push(format!("delete the code that reads {dotted} ({}); `spira-lint` names what still does", field.to_ascii_uppercase()));
    Ok(rep)
}

/// Retires scheduler task `name`: drops it from the `SPIRA_TASK_MODES` default and names it in `removed_tasks`.
pub fn retire_task(root: &Path, name: &str, bead: &str) -> Result<Report, String> {
    let reg = read(root, TASK_MODES)?;
    let at = reg
        .match_indices(&format!("{name}="))
        .map(|(i, _)| i)
        .find(|&i| i > 0 && matches!(reg.as_bytes()[i - 1], b' ' | b'\n' | b'='))
        .ok_or_else(|| format!("{TASK_MODES} registers no task {name:?}"))?;
    let after = reg[at..].find([' ', '\n', '}']).map_or(reg.len(), |n| at + n);
    let (from, to) = match (reg.as_bytes().get(after), reg.as_bytes()[at - 1]) {
        (Some(b' ' | b'\n'), b'\n') => (at, after + 1),
        (Some(b' '), _) => (at, after + 1),
        (_, b' ') => (at - 1, after),
        _ => (at, after),
    };
    let edited = format!("{}{}", &reg[..from], &reg[to..]);
    let (frag, text) = fragment_list(root, bead, "removed_tasks", name)?;
    write(root, TASK_MODES, &edited)?;
    write_fragment(root, &frag, &text)?;

    let mut rep = Report::default();
    rep.changed.push(format!("{TASK_MODES}: {name} dropped from the default"));
    rep.changed.push(format!("{frag}: {name} added to removed_tasks"));
    rep.left.push(format!("delete the code that registers task {name:?} with the scheduler, and any unit or test that names it"));
    Ok(rep)
}
