//! `rename-vocabulary`'s plan: which statutes in force hold a retired term and what each becomes.

use std::collections::BTreeMap;

use spira_lint::rules::ffx_vocabulary::rewrite;

use crate::code::{Part, Pointers};
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
