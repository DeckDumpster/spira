//! `rename-vocabulary`'s plan: which statutes in force hold a retired term and what each becomes.

use std::collections::BTreeMap;

use spira_lint::rules::ffx_vocabulary::rewrite;

use crate::code::{self, Part, Pointers};
use crate::vocabulary_terms;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Amend { key: String, text: String },
    Rename { from: String, to: String, text: String, numbered: bool },
    Unresolved { key: String, terms: Vec<String> },
    Collision { from: String, to: String },
}

pub type Plan = Vec<Entry>;

fn numbered(slug: &str, toc: &[Part], pointers: &Pointers) -> bool {
    toc.iter().any(|p| p.slugs.iter().any(|s| s == slug))
        || pointers.moved.iter().any(|m| m.slug == slug)
        || pointers.merged.iter().any(|m| m.from == slug || m.into == slug)
        || pointers.retired.iter().any(|r| r.slug == slug || r.replaced_by.as_deref() == Some(slug))
}

pub fn plan(book: &BTreeMap<String, String>, toc: &[Part], pointers: &Pointers) -> Plan {
    let mut out = Plan::new();
    let mut taken: Vec<String> = Vec::new();
    for (key, text) in book.iter().filter(|(k, _)| k.starts_with("law-")) {
        let terms = vocabulary_terms(key, text);
        if terms.is_empty() {
            continue;
        }
        let (new_key, new_text) = (rewrite(key), rewrite(text));
        if !vocabulary_terms(&new_key, &new_text).is_empty() {
            out.push(Entry::Unresolved { key: key.clone(), terms });
        } else if new_key == *key {
            out.push(Entry::Amend { key: key.clone(), text: new_text });
        } else if book.contains_key(&new_key) || taken.contains(&new_key) {
            out.push(Entry::Collision { from: key.clone(), to: new_key });
        } else {
            taken.push(new_key.clone());
            out.push(Entry::Rename { from: key.clone(), numbered: numbered(key, toc, pointers), to: new_key, text: new_text });
        }
    }
    out
}

fn changed_sentences(old: &str, new: &str) -> String {
    let mut out = String::new();
    for (o, n) in old.split_inclusive(". ").zip(new.split_inclusive(". ")).filter(|(o, n)| o != n) {
        out.push_str(&format!("- {}\n+ {}\n", o.trim_end(), n.trim_end()));
    }
    out
}

/// The dry run's listing: each statute the plan writes, with every changed sentence old then new.
pub fn dry_run_report(before: &BTreeMap<String, String>, plan: &Plan) -> String {
    let mut out = String::new();
    for e in plan {
        let (head, old, new) = match e {
            Entry::Amend { key, text } => (key.clone(), before.get(key), text),
            Entry::Rename { from, to, text, .. } => (format!("{from} → {to}"), before.get(from), text),
            _ => continue,
        };
        out.push_str(&head);
        out.push('\n');
        out.push_str(&changed_sentences(old.map_or("", String::as_str), new));
        out.push('\n');
    }
    out
}

/// The toc and pointers after `renamed` (from, to, numbered) was written: an unnumbered old slug
/// is numbered first so the pointer naming it passes `code::check_pointers`.
pub fn record_renames(toc: &[Part], pointers: &Pointers, renamed: &[(String, String, bool)], today: &str) -> Result<(Vec<Part>, Pointers), String> {
    let mut toc = toc.to_vec();
    let mut pointers = pointers.clone();
    for (from, to, numbered) in renamed {
        if !numbered {
            toc = code::append(&toc, 1, from)?;
        }
        pointers.retired.push(code::Retired { slug: from.clone(), reason: "renamed out of retired vocabulary".into(), replaced_by: Some(to.clone()), on: today.into() });
    }
    Ok((toc, pointers))
}
