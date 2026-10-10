//! Catalog search text and query construction.
//!
//! The index holds one normalized document per live work, built from its
//! display title, retired aliases' titles, the scanned origin title, effective
//! contributor names and tags. Queries are normalized the same way and become
//! quoted FTS5 prefix terms, so user text can never inject query syntax.
use crate::matching::normalize_title as normalize;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_TERMS: usize = 16;
pub const MAX_TERM: usize = 64;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    pub title: String,
    pub aliases: Vec<String>,
    pub people: Vec<String>,
    pub tags: Vec<String>,
}

/// The indexed body: normalized, de-duplicated words in a stable order.
pub fn body(document: &Document) -> String {
    let mut seen = BTreeSet::new();
    let mut words = Vec::new();
    let fields = std::iter::once(&document.title)
        .chain(&document.aliases)
        .chain(&document.people)
        .chain(&document.tags);
    for field in fields {
        // Both elision forms: "d'Amélie" indexes as "damélie" and "d amélie",
        // "Director's" as "directors" and "director s".
        let joined = normalize(field);
        let split = normalize(&field.replace(['\'', '’'], " "));
        for word in joined
            .split(' ')
            .chain(split.split(' '))
            .filter(|w| !w.is_empty())
        {
            if seen.insert(word.to_string()) {
                words.push(word.to_string());
            }
        }
    }
    words.join(" ")
}

/// Normalized query terms; empty when nothing searchable remains.
pub fn terms(query: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    normalize(query)
        .split(' ')
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().take(MAX_TERM).collect::<String>())
        .filter(|t| seen.insert(t.clone()))
        .take(MAX_TERMS)
        .collect()
}

/// FTS5 MATCH expression: every term must match as a prefix. Terms are
/// normalized alphanumerics, quoted, so no operator or column syntax survives.
pub fn fts_query(terms: &[String]) -> Option<String> {
    if terms.is_empty() {
        return None;
    }
    Some(
        terms
            .iter()
            .map(|t| format!("\"{}\"*", t.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" AND "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn documents_and_queries_normalize_identically() {
        let doc = Document {
            title: "Amélie".into(),
            aliases: vec!["Le Fabuleux Destin d'Amélie Poulain".into()],
            people: vec!["Audrey Tautou".into()],
            tags: vec!["favorite".into(), "Favorite".into()],
        };
        assert_eq!(
            body(&doc),
            "amélie le fabuleux destin damélie poulain d audrey tautou favorite"
        );
        assert_eq!(terms("  AMÉLIE  tautou amélie "), ["amélie", "tautou"]);
        assert_eq!(
            fts_query(&terms("audrey tau")).unwrap(),
            "\"audrey\"* AND \"tau\"*"
        );
        // Query syntax is not passed through.
        assert_eq!(
            fts_query(&terms("title:x OR \"y\" NEAR(z)")).unwrap(),
            "\"title\"* AND \"x\"* AND \"or\"* AND \"y\"* AND \"near\"* AND \"z\"*"
        );
        assert_eq!(fts_query(&terms("!!! ---")), None);
        assert_eq!(terms(&"a ".repeat(40)).len(), 1);
        assert_eq!(
            terms(&(0..40).map(|i| format!("w{i} ")).collect::<String>()).len(),
            MAX_TERMS
        );
    }
}
