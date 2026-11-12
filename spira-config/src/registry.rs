//! The config key registry — `spira/conf.d/<KEY>`, one file per key (sp-g3uwp). Read-only
//! here: this module mirrors `conf-gen.sh`'s own parser and topological sort so
//! [`crate::resolve::resolve`] can apply each key's generated default in-process, without
//! shelling out to bash for the 222 keys whose default is a plain `: "${KEY:=...}"`
//! statement. Never writes `conf.d` — enact a key by adding or editing its file under
//! `spira/conf.d/`, never here.
//!
//! WHAT THIS DOES NOT COVER (same fence `conf-gen.sh` itself names): a key whose `DEFAULT`
//! body carries no real statement — a `PROCEDURAL` stub (ordering constraints) or a
//! `NO DEFAULT` stub — contributes only its name to the allowlist. `spira::resolve::resolve`
//! hand-ports the procedural keys directly (see its module doc) and defaults a no-default key
//! to the empty string, exactly as `spira_conf_defaults` does today.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

/// One `spira/conf.d/<KEY>` file, parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct RegistryKey {
    pub name: String,
    pub ty: String,
    pub group: String,
    pub doc: String,
    /// `SCHEMA=REQUIRED: …`: no default, and the key must be set in the config for the box to run.
    pub required: bool,
    /// The raw text between the `DEFAULT<<'SPIRA_CONF_DEFAULT_EOF'` / `SPIRA_CONF_DEFAULT_EOF`
    /// markers, exactly as `conf-gen.sh`'s own `parse_one` reads it (comments and at most one
    /// `: "${KEY:=...}"` statement).
    pub default_body: String,
    /// The `spira.toml` spelling under `[spira]`: `SPIRA_FOO` is `foo`, `COCKPIT_FOO` is `cockpit_foo`.
    pub field: String,
    /// Declared in `conf.toml.d`: a key only `spira.toml` carries.
    pub toml_only: bool,
    /// `MAX=`: the hard ceiling `validate` enforces on an integer value.
    pub max: Option<f64>,
    /// `SCHEMA=`: the key's description in the JSON Schema.
    pub schema_doc: Option<String>,
}

/// Every TYPE a registry file may declare.
pub const TYPES: &[&str] = &["string", "u32", "u64", "bool", "list", "onoff", "czar_stage"];

/// The `spira.toml` field name of a registry key.
pub fn field_of(key: &str) -> String {
    key.strip_prefix("SPIRA_").unwrap_or(key).to_ascii_lowercase()
}

impl RegistryKey {
    /// The one `: "${KEY:=EXPR}"` statement in [`default_body`](Self::default_body), if it
    /// carries one — `None` for a `PROCEDURAL` or `NO DEFAULT` stub, which is allowlist-only
    /// (`conf-gen.sh`: "a DEFAULT body with no statement in it is read as allowlist membership
    /// only — not an error"). Only the `:=` form appears anywhere in the registry today (the
    /// `=` preserve-empty form is used only by the hand-written keys `conf.d`'s PROCEDURAL
    /// stubs name); a key that ever grows one would need this parser extended to match.
    pub fn default_expr(&self) -> Option<&str> {
        let needle = format!("${{{}:=", self.name);
        let idx = self.default_body.find(&needle)?;
        let start = idx + needle.len();
        extract_braced(&self.default_body, start)
    }
}

/// Scans forward from `start` (the byte offset right after an already-consumed, unmatched
/// `{`) for the matching `}`, tracking brace depth so a nested `${...}` inside a default
/// (`${SPIRA_WIKI:-$SPIRA_REPO}`'s own braces notwithstanding — none of today's registry
/// defaults nest a SECOND `${` inside the first, but this stays depth-aware rather than
/// "find the next `}`" in case one ever does) is not mistaken for the close. Returns the text
/// strictly between, or `None` if the braces never balance.
fn extract_braced(s: &str, start: usize) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut depth: i32 = 1;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..i]);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The config key registry of one release: `<home>/conf.d` and `<home>/conf.toml.d`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Registry {
    pub keys: BTreeMap<String, RegistryKey>,
}

impl Registry {
    /// The registry of `home`: from its [`INDEX_FILE`] when it has one, else from both
    /// directories. A stale index is refused by name, never passed over for the directories.
    pub fn load(home: &Path) -> Result<Registry, String> {
        let index = home.join(INDEX_FILE);
        match fs::symlink_metadata(&index) {
            Ok(_) => Registry::from_index(home, &index),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Registry::from_dirs(home),
            Err(e) => Err(format!("{}: {e}", index.display())),
        }
    }

    /// Reads both directories of `home`; a directory that cannot be read is a named refusal.
    pub fn from_dirs(home: &Path) -> Result<Registry, String> {
        Registry::from_keys(load(&home.join(SOURCE_DIRS[0]))?, load(&home.join(SOURCE_DIRS[1]))?)
    }

    fn from_keys(mut keys: BTreeMap<String, RegistryKey>, toml_only: BTreeMap<String, RegistryKey>) -> Result<Registry, String> {
        for (name, mut key) in toml_only {
            key.toml_only = true;
            if keys.insert(name.clone(), key).is_some() {
                return Err(format!("{name} is declared in both conf.d and conf.toml.d"));
            }
        }
        let mut fields = BTreeMap::new();
        for k in keys.values() {
            if let Some(other) = fields.insert(k.field.clone(), k.name.clone()) {
                return Err(format!("{}: field {} is declared twice in the registry ({other})", k.name, k.field));
            }
        }
        Ok(Registry { keys })
    }

    fn from_index(home: &Path, index: &Path) -> Result<Registry, String> {
        let text = read_counted(index).map_err(|e| format!("{}: {e}", index.display()))?;
        let parsed = parse_index(&text).map_err(|e| format!("{}: unreadable registry index: {e}", index.display()))?;
        let now = stamp(home, parsed.sources.iter().map(|(dir, name, _)| (*dir, *name)));
        if now != parsed.stamp {
            return Err(format!(
                "{} is stale: it was derived from sources stamped {}, and {} is now stamped {now} — a release must be rebuilt; in a tree, remove the index to read conf.d itself",
                index.display(),
                parsed.stamp,
                home.display()
            ));
        }
        Registry::from_sources(parsed.sources.into_iter())
    }

    fn from_sources<'a>(sources: impl Iterator<Item = (&'a str, &'a str, &'a str)>) -> Result<Registry, String> {
        let mut dirs: [BTreeMap<String, RegistryKey>; 2] = Default::default();
        for (dir, name, body) in sources {
            dirs[usize::from(dir == SOURCE_DIRS[1])].insert(name.to_string(), parse_one(name, body)?);
        }
        let [keys, toml_only] = dirs;
        Registry::from_keys(keys, toml_only)
    }

    pub fn get(&self, key: &str) -> Option<&RegistryKey> {
        self.keys.get(key)
    }

    pub fn by_field(&self, field: &str) -> Option<&RegistryKey> {
        self.keys.values().find(|k| k.field == field)
    }
}

/// Whether `home` has a source file for `key`: two `stat`s, not the registry load, which stamps
/// every source to trust its index.
pub fn declares_key(home: &Path, key: &str) -> bool {
    SOURCE_DIRS.iter().any(|d| home.join(d).join(key).is_file())
}

/// This process's registry, read once from the release [`crate::resolve::locate_home_for_process`]
/// finds. A refusal is not remembered: the next call looks again.
pub fn for_process() -> Result<&'static Registry, String> {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    if let Some(r) = REGISTRY.get() {
        return Ok(r);
    }
    let loaded = Registry::load(&crate::resolve::locate_home_for_process()?)?;
    Ok(REGISTRY.get_or_init(|| loaded))
}

/// The registry index a release carries beside `conf.d`: every source file of `conf.d` and
/// `conf.toml.d` in one file, read with one open instead of one per key.
pub const INDEX_FILE: &str = "conf.d.index";
const INDEX_HEADER: &str = "spira-registry-index 1";
const SOURCE_DIRS: [&str; 2] = ["conf.d", "conf.toml.d"];

static FILES_OPENED: AtomicUsize = AtomicUsize::new(0);

/// Config files this process has opened to read: `spira.toml`'s layers and the registry's sources.
pub fn files_opened() -> usize {
    FILES_OPENED.load(Ordering::Relaxed)
}

pub(crate) fn read_counted(path: &Path) -> std::io::Result<String> {
    FILES_OPENED.fetch_add(1, Ordering::Relaxed);
    fs::read_to_string(path)
}

struct ParsedIndex<'a> {
    stamp: String,
    sources: Vec<(&'a str, &'a str, &'a str)>,
}

fn parse_index(text: &str) -> Result<ParsedIndex<'_>, String> {
    let (header, rest) = text.split_once('\n').ok_or("no header")?;
    if header != INDEX_HEADER {
        return Err(format!("header {header:?} is not {INDEX_HEADER:?}"));
    }
    let (stamp_line, mut rest) = rest.split_once('\n').ok_or("no stamp")?;
    let stamp = stamp_line.strip_prefix("stamp ").ok_or("no stamp")?.to_string();
    let mut sources = Vec::new();
    while !rest.is_empty() {
        let (head, after) = rest.split_once('\n').ok_or("a source record has no header line")?;
        let (path, len) = head.rsplit_once(' ').ok_or_else(|| format!("source record {head:?} has no length"))?;
        let len: usize = len.parse().map_err(|_| format!("source record {head:?} has no length"))?;
        let (dir, name) = path.split_once('/').filter(|(d, _)| SOURCE_DIRS.contains(d)).ok_or_else(|| format!("source {path:?} is not under {}", SOURCE_DIRS.join(" or ")))?;
        let body = after.get(..len).ok_or_else(|| format!("source {path:?} is cut short"))?;
        sources.push((dir, name, body));
        rest = &after[len..];
    }
    Ok(ParsedIndex { stamp, sources })
}

/// What a source's content cannot change without changing: each directory's mtime (a key added,
/// removed or renamed) and each file's length and mtime, hashed. Taken with `stat`, never by
/// reading a file; a source that cannot be stated stamps as its error, so it reads as stale.
fn stamp<'a>(home: &Path, sources: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    use std::os::unix::fs::MetadataExt;
    let mut h = Fnv::default();
    let mut add = |rel: &str| {
        let line = match fs::metadata(home.join(rel)) {
            Ok(m) => format!("{rel} {} {}.{}\n", if m.is_dir() { 0 } else { m.len() }, m.mtime(), m.mtime_nsec()),
            Err(e) => format!("{rel} {e}\n"),
        };
        h.write(line.as_bytes());
    };
    for d in SOURCE_DIRS {
        add(d);
    }
    for (dir, name) in sources {
        add(&format!("{dir}/{name}"));
    }
    format!("{:016x}", h.0)
}

/// FNV-1a: a hash whose value is the same in every build, so an index written by one binary is
/// read by another.
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
}

/// Writes `<home>/`[`INDEX_FILE`] from `home`'s two registry directories, then reads it back
/// and refuses (removing it) unless it loads the registry the directories hold. The sources are
/// read on both sides of the stamp, so an edit made while indexing is refused, never indexed.
pub fn write_index(home: &Path) -> Result<PathBuf, String> {
    let read_all = || -> Result<Vec<(&'static str, String, String)>, String> {
        let mut all = Vec::new();
        for dir in SOURCE_DIRS {
            all.extend(read_sources(&home.join(dir))?.into_iter().map(|(name, text)| (dir, name, text)));
        }
        Ok(all)
    };
    let sources = read_all()?;
    let stamp = stamp(home, sources.iter().map(|(d, n, _)| (*d, n.as_str())));
    if read_all()? != sources {
        return Err(format!("{}: the registry changed while it was being indexed — run it again", home.display()));
    }
    let from_dirs = Registry::from_sources(sources.iter().map(|(d, n, t)| (*d, n.as_str(), t.as_str())))?;
    let mut out = format!("{INDEX_HEADER}\nstamp {stamp}\n");
    for (dir, name, text) in &sources {
        out.push_str(&format!("{dir}/{name} {}\n{text}", text.len()));
    }
    let index = home.join(INDEX_FILE);
    let tmp = home.join(format!(".{INDEX_FILE}.{}", std::process::id()));
    fs::write(&tmp, &out).and_then(|()| fs::rename(&tmp, &index)).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("{}: {e}", index.display())
    })?;
    match Registry::load(home) {
        Ok(r) if r == from_dirs => Ok(index),
        other => {
            let _ = fs::remove_file(&index);
            let why = other.err().unwrap_or_else(|| "it loads different keys".to_string());
            Err(format!("{}: the index does not load the registry its directories hold ({why}) — removed", index.display()))
        }
    }
}

/// Parses every file directly inside `dir` the way `conf-gen.sh`'s `parse_one` does: `TYPE=`,
/// `GROUP=`, `DOC=` lines, then a `DEFAULT<<'SPIRA_CONF_DEFAULT_EOF'` heredoc closed by a line
/// that is exactly `SPIRA_CONF_DEFAULT_EOF`. A filename that is not an uppercase `KEY` (no
/// lowercase, no punctuation beyond `_`) is skipped, mirroring `parse_one`'s own `case "$key"
/// in [A-Z_]*` guard — a directory holding `.gitkeep` or similar must not become a spurious
/// key.
///
/// A MISSING DIRECTORY IS A NAMED ERROR, NOT AN EMPTY REGISTRY (sp-1cdgq round 3). It used
/// to return `Ok(empty)` so the caller (`resolve`) could fall back to its ~29 hand-written
/// keys alone — meant for "a release that has not shipped `conf.d` yet" but in practice hit
/// by a test fixture's `--home` that simply forgot to copy `conf.d` in, twice
/// (sp-8qm8g, then sp-1cdgq itself): the whole generic registry pass silently resolved to
/// nothing, with no error anywhere, and a config knob the fixture's own `export` believed it
/// had set (`SPIRA_MAX_DWARVES`, `SPIRA_CALL_JITTER`) never reached `Conf`. Production always
/// ships `conf.d` beside the release's binaries, so this changes nothing real — it only turns
/// a silently-degraded test fixture into a named failure the first time it is run, which is
/// the whole point. An EXISTING-but-empty directory is still not an error (conf-gen.sh itself
/// is the one place that is fatal, since it would otherwise regenerate an empty allowlist);
/// only a directory `read_dir` cannot open at all — missing, or something else wrong with it —
/// is refused.
pub fn load(dir: &Path) -> Result<BTreeMap<String, RegistryKey>, String> {
    read_sources(dir)?.into_iter().map(|(name, text)| parse_one(&name, &text).map(|k| (name, k))).collect()
}

fn read_sources(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let entries = fs::read_dir(dir).map_err(|_| format!("no config registry at {}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if !name.chars().all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit()) {
            continue;
        }
        let text = read_counted(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        out.push((name, text));
    }
    out.sort();
    Ok(out)
}

fn parse_one(name: &str, text: &str) -> Result<RegistryKey, String> {
    let mut ty = String::new();
    let mut group = String::new();
    let mut doc = String::new();
    let mut required = false;
    let mut max = None;
    let mut schema_doc = None;
    let mut body = String::new();
    let mut in_default = false;
    let mut closed = true;
    for line in text.lines() {
        if in_default {
            if line == "SPIRA_CONF_DEFAULT_EOF" {
                in_default = false;
                closed = true;
                continue;
            }
            body.push_str(line);
            body.push('\n');
            continue;
        }
        if let Some(v) = line.strip_prefix("TYPE=") {
            ty = v.to_string();
        } else if let Some(v) = line.strip_prefix("GROUP=") {
            group = v.to_string();
        } else if let Some(v) = line.strip_prefix("DOC=") {
            doc = v.to_string();
        } else if let Some(v) = line.strip_prefix("SCHEMA=") {
            required = v.starts_with("REQUIRED:");
            schema_doc = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("MAX=") {
            max = Some(v.trim().parse::<f64>().map_err(|e| format!("{name}: MAX={v:?}: {e}"))?);
        } else if line == "DEFAULT<<'SPIRA_CONF_DEFAULT_EOF'" {
            in_default = true;
            closed = false;
        }
        // An unrecognised line is ignored, same as conf-gen.sh (which only warns to stderr).
    }
    if !closed {
        return Err(format!("{name}: DEFAULT heredoc never closed (missing SPIRA_CONF_DEFAULT_EOF)"));
    }
    if !TYPES.contains(&ty.trim()) {
        return Err(format!("{name}: TYPE={ty:?} is not one of {}", TYPES.join(" ")));
    }
    Ok(RegistryKey {
        name: name.to_string(),
        ty: ty.trim().to_string(),
        group,
        doc,
        required,
        default_body: body,
        field: field_of(name),
        toml_only: false,
        max,
        schema_doc,
    })
}

/// The keys that carry a real default statement, in the same deterministic topological order
/// `conf-gen.sh` writes them in (Kahn's algorithm, alphabetical tie-break) — a key's default
/// always comes after any other registry key its own default text names, regardless of this
/// directory's merely-alphabetical file order. Refuses (rather than guessing an order) on a
/// dependency cycle, same as `conf-gen.sh`.
pub fn topo_order(registry: &BTreeMap<String, RegistryKey>) -> Result<Vec<String>, String> {
    // A key depends on every OTHER defaulted key its default text names, and only on defaulted
    // ones: a hand-written key is named in plenty of defaults but is no node, and counting it
    // would leave an in-degree no round could bring to zero, reading as a cycle. Every process
    // resolves config through here, so each default is parsed once and a key is only looked
    // for in a default that could name one.
    let nodes: Vec<(&str, &str)> = registry.iter().filter_map(|(n, k)| k.default_expr().map(|e| (n.as_str(), e))).collect();
    let mut indeg = vec![0usize; nodes.len()];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (i, (_, expr)) in nodes.iter().enumerate() {
        if !expr.bytes().any(|b| b.is_ascii_uppercase() || b == b'_') {
            continue;
        }
        for (j, (name, _)) in nodes.iter().enumerate() {
            if i != j && expr.contains(name) {
                indeg[i] += 1;
                dependents[j].push(i);
            }
        }
    }

    let mut done = vec![false; nodes.len()];
    let mut order: Vec<String> = Vec::with_capacity(nodes.len());
    while order.len() < nodes.len() {
        let avail: Vec<usize> = (0..nodes.len()).filter(|&i| !done[i] && indeg[i] == 0).collect();
        if avail.is_empty() {
            let cyc: Vec<&str> = (0..nodes.len()).filter(|&i| !done[i]).map(|i| nodes[i].0).collect();
            return Err(format!("conf.d: dependency cycle among registry defaults: {}", cyc.join(", ")));
        }
        for &k in &avail {
            done[k] = true;
            order.push(nodes[k].0.to_string());
            for &o in &dependents[k] {
                indeg[o] -= 1;
            }
        }
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_home_missing_either_directory_is_refused_by_path() {
        let dir = testkit::TempDir::new("spira-config-registry-home");
        let conf_d = dir.join("conf.d");
        std::fs::create_dir_all(&conf_d).unwrap();
        assert_eq!(Registry::load(dir.path()).unwrap_err(), format!("no config registry at {}", dir.join("conf.toml.d").display()));
        std::fs::remove_dir(&conf_d).unwrap();
        std::fs::create_dir_all(dir.join("conf.toml.d")).unwrap();
        assert_eq!(Registry::load(dir.path()).unwrap_err(), format!("no config registry at {}", conf_d.display()));
    }

    #[test]
    fn an_unknown_type_is_refused() {
        let dir = testkit::TempDir::new("spira-config-registry-type");
        std::fs::write(dir.join("SPIRA_X"), "TYPE=float\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\nSPIRA_CONF_DEFAULT_EOF\n").unwrap();
        assert!(load(dir.path()).unwrap_err().contains("TYPE=\"float\""));
    }

    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn declares_key_sees_a_source_in_either_directory_and_not_an_absent_one() {
        let dir = testkit::TempDir::new("declares-key");
        for d in SOURCE_DIRS {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        std::fs::write(dir.path().join("conf.d/SPIRA_A"), "").unwrap();
        std::fs::write(dir.path().join("conf.toml.d/SPIRA_B"), "").unwrap();
        assert!(declares_key(dir.path(), "SPIRA_A") && declares_key(dir.path(), "SPIRA_B"));
        assert!(!declares_key(dir.path(), "SPIRA_C"));
    }

    fn key(name: &str, body: &str) -> RegistryKey {
        RegistryKey {
            name: name.to_string(),
            ty: "string".into(),
            group: "test".into(),
            doc: String::new(),
            required: false,
            default_body: body.to_string(),
            field: field_of(name),
            toml_only: false,
            max: None,
            schema_doc: None,
        }
    }

    /// sp-1cdgq round 3: a missing directory is now a NAMED error, not a silent empty
    /// registry — this is the test that used to assert the opposite. Two base reds
    /// (sp-8qm8g, sp-1cdgq) came from a test fixture's `--home` lacking `conf.d`, and
    /// `resolve()` quietly proceeding on the ~29 hand-written keys alone masked both until
    /// something that specifically needed a registry-resolved key went looking.
    #[test]
    fn missing_directory_is_a_named_error_not_an_empty_registry() {
        let dir = testkit::TempDir::new("spira-config-registry-missing");
        let missing = dir.join("does-not-exist");
        let err = load(&missing).unwrap_err();
        assert_eq!(err, format!("no config registry at {}", missing.display()));
    }

    /// An EXISTING but empty directory is still not an error — a release that genuinely
    /// ships zero registry files (or a fixture that only cares about the hand-written keys)
    /// keeps working. Only "cannot even read this path" is refused.
    #[test]
    fn existing_empty_directory_is_still_an_empty_registry() {
        let dir = testkit::TempDir::new("spira-config-registry-empty");
        let empty = dir.join("conf.d");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(load(&empty).unwrap(), BTreeMap::new());
    }

    #[test]
    fn parses_type_group_doc_and_default_body() {
        let dir = testkit::TempDir::new("spira-config-registry-parse");
        std::fs::write(
            dir.join("SPIRA_EXAMPLE"),
            "TYPE=u32\nGROUP=test\nDOC=an example\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n    : \"${SPIRA_EXAMPLE:=7}\"\nSPIRA_CONF_DEFAULT_EOF\n",
        )
        .unwrap();
        let reg = load(dir.path()).unwrap();
        let k = &reg["SPIRA_EXAMPLE"];
        assert_eq!(k.ty, "u32");
        assert_eq!(k.group, "test");
        assert_eq!(k.doc, "an example");
        assert_eq!(k.default_expr(), Some("7"));
    }

    #[test]
    fn a_lowercase_or_dotfile_name_is_skipped() {
        let dir = testkit::TempDir::new("spira-config-registry-skip");
        std::fs::write(dir.join(".gitkeep"), "").unwrap();
        std::fs::write(dir.join("readme.md"), "").unwrap();
        let reg = load(dir.path()).unwrap();
        assert!(reg.is_empty());
    }

    #[test]
    fn unterminated_heredoc_is_refused() {
        let dir = testkit::TempDir::new("spira-config-registry-unterminated");
        std::fs::write(dir.join("SPIRA_BAD"), "DEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\nno close\n").unwrap();
        assert!(load(dir.path()).is_err());
    }

    #[test]
    fn procedural_and_no_default_stubs_have_no_expr() {
        let k1 = key("SPIRA_X", "    # PROCEDURAL — ...\n");
        let k2 = key("SPIRA_Y", "    # NO DEFAULT.\n");
        assert_eq!(k1.default_expr(), None);
        assert_eq!(k2.default_expr(), None);
    }

    #[test]
    fn default_expr_is_brace_balanced_for_a_nested_substitution() {
        let k = key(
            "SPIRA_OVERRIDES",
            "    : \"${SPIRA_OVERRIDES:=${XDG_CONFIG_HOME:-$HOME/.config}/spira/overrides}\"\n",
        );
        assert_eq!(
            k.default_expr(),
            Some("${XDG_CONFIG_HOME:-$HOME/.config}/spira/overrides")
        );
    }

    #[test]
    fn topo_order_runs_a_dependency_after_what_it_names() {
        let mut reg = BTreeMap::new();
        reg.insert("SPIRA_B".to_string(), key("SPIRA_B", "    : \"${SPIRA_B:=$SPIRA_A/x}\"\n"));
        reg.insert("SPIRA_A".to_string(), key("SPIRA_A", "    : \"${SPIRA_A:=1}\"\n"));
        let order = topo_order(&reg).unwrap();
        let pos_a = order.iter().position(|k| k == "SPIRA_A").unwrap();
        let pos_b = order.iter().position(|k| k == "SPIRA_B").unwrap();
        assert!(pos_a < pos_b, "{order:?}");
    }

    #[test]
    fn topo_order_refuses_a_cycle() {
        let mut reg = BTreeMap::new();
        reg.insert("SPIRA_A".to_string(), key("SPIRA_A", "    : \"${SPIRA_A:=$SPIRA_B}\"\n"));
        reg.insert("SPIRA_B".to_string(), key("SPIRA_B", "    : \"${SPIRA_B:=$SPIRA_A}\"\n"));
        assert!(topo_order(&reg).is_err());
    }

    #[test]
    fn stub_keys_are_excluded_from_topo_order() {
        let mut reg = BTreeMap::new();
        reg.insert("SPIRA_A".to_string(), key("SPIRA_A", "    : \"${SPIRA_A:=1}\"\n"));
        reg.insert("SPIRA_STUB".to_string(), key("SPIRA_STUB", "    # NO DEFAULT.\n"));
        let order = topo_order(&reg).unwrap();
        assert_eq!(order, vec!["SPIRA_A".to_string()]);
    }
}
