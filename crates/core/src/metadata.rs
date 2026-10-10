use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contribution {
    pub values: BTreeMap<String, Value>,
    pub tags: BTreeSet<String>,
    /// Only local contributions may suppress imported tags.
    pub excluded_tags: BTreeSet<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolved {
    pub values: BTreeMap<String, Value>,
    pub field_sources: BTreeMap<String, Vec<String>>,
    pub tags: BTreeSet<String>,
    pub tag_sources: BTreeMap<String, Vec<String>>,
    /// Disagreeing providers without a local override are exposed, never silently ranked.
    pub conflicts: BTreeMap<String, BTreeMap<String, Value>>,
}
pub fn resolve(documents: &BTreeMap<String, Contribution>) -> Resolved {
    let mut result = Resolved::default();
    let local = documents.get("local");
    let fields: BTreeSet<_> = documents
        .values()
        .flat_map(|d| d.values.keys().cloned())
        .collect();
    for field in fields {
        if let Some(value) = local.and_then(|d| d.values.get(&field)) {
            result.values.insert(field.clone(), value.clone());
            result.field_sources.insert(field, vec!["local".into()]);
            continue;
        }
        let candidates: BTreeMap<_, _> = documents
            .iter()
            .filter(|(source, _)| source.as_str() != "scan" && source.as_str() != "local")
            .filter_map(|(source, d)| d.values.get(&field).map(|v| (source.clone(), v.clone())))
            .collect();
        if let Some(value) = candidates.values().next() {
            if candidates.values().all(|v| v == value) {
                result.values.insert(field.clone(), value.clone());
                result
                    .field_sources
                    .insert(field, candidates.keys().cloned().collect());
            } else {
                result.conflicts.insert(field, candidates);
            }
        } else if let Some(value) = documents.get("scan").and_then(|d| d.values.get(&field)) {
            result.values.insert(field.clone(), value.clone());
            result.field_sources.insert(field, vec!["scan".into()]);
        }
    }
    for (source, document) in documents {
        for tag in &document.tags {
            if local.is_some_and(|d| d.excluded_tags.contains(tag)) {
                continue;
            }
            result.tags.insert(tag.clone());
            result
                .tag_sources
                .entry(tag.clone())
                .or_default()
                .push(source.clone());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provenance_override_conflict_and_tag_removal() {
        let mut docs = BTreeMap::new();
        let doc = |title: &str, tags: &[&str]| Contribution {
            values: BTreeMap::from([("title".into(), Value::String(title.into()))]),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        docs.insert("scan".into(), doc("Filename", &[]));
        docs.insert("catabolic".into(), doc("Imported", &["favorite", "drama"]));
        docs.insert("local".into(), doc("My title", &["favorite"]));
        assert_eq!(resolve(&docs).values["title"], "My title");
        docs.insert("catabolic".into(), doc("Refreshed", &["drama"]));
        assert!(resolve(&docs).tags.contains("favorite"));
        docs.get_mut("local")
            .unwrap()
            .excluded_tags
            .insert("drama".into());
        assert!(!resolve(&docs).tags.contains("drama"));
        docs.get_mut("local").unwrap().values.clear();
        assert_eq!(resolve(&docs).values["title"], "Refreshed");
        docs.insert("other".into(), doc("Disagreement", &[]));
        assert!(resolve(&docs).conflicts.contains_key("title"));
        assert!(!resolve(&docs).values.contains_key("title"));
    }
}

/// Shared conventional fields; extension fields retain arbitrary non-null JSON.
pub fn valid_field(key: &str, value: &Value) -> bool {
    fn text(value: &Value, max: usize) -> bool {
        value
            .as_str()
            .is_some_and(|s| !s.trim().is_empty() && s.len() <= max)
    }
    match key {
        "title" => text(value, 500),
        "description" => value.as_str().is_some_and(|s| s.len() <= 8000),
        "release_year" => value.as_i64().is_some_and(|y| (1..=9999).contains(&y)),
        "release_date" => value.as_str().is_some_and(|s| {
            let bytes = s.as_bytes();
            if bytes.len() != 10
                || bytes[4] != b'-'
                || bytes[7] != b'-'
                || bytes
                    .iter()
                    .enumerate()
                    .any(|(i, b)| i != 4 && i != 7 && !b.is_ascii_digit())
            {
                return false;
            }
            let year = s[..4].parse::<u32>().unwrap();
            let month = s[5..7].parse::<usize>().unwrap();
            let day = s[8..].parse::<u32>().unwrap();
            let leap =
                year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
            let days = [
                31,
                if leap { 29 } else { 28 },
                31,
                30,
                31,
                30,
                31,
                31,
                30,
                31,
                30,
                31,
            ];
            year > 0 && (1..=12).contains(&month) && day > 0 && day <= days[month - 1]
        }),
        "cast" => value.as_array().is_some_and(|cast| {
            cast.len() <= 100
                && cast.iter().all(|person| {
                    person.as_object().is_some_and(|p| {
                        p.get("name").is_some_and(|v| text(v, 200))
                            && p.get("role").is_none_or(|v| text(v, 200))
                            && p.keys().all(|k| k == "name" || k == "role")
                    })
                })
        }),
        _ => !value.is_null(),
    }
}
#[cfg(test)]
mod field_tests {
    use super::*;
    #[test]
    fn conventional_fields_reject_invalid_dates_and_shapes() {
        assert!(valid_field(
            "release_date",
            &serde_json::json!("2024-02-29")
        ));
        for date in [
            "2025-02-29",
            "2024-13-01",
            "0000-01-01",
            "2024-00-01",
            "2024-01-00",
            "💥-01-01",
        ] {
            assert!(!valid_field("release_date", &serde_json::json!(date)));
        }
        assert!(!valid_field("release_year", &serde_json::json!(2024.5)));
        assert!(!valid_field("cast", &serde_json::json!([{"role":"Host"}])));
        assert!(valid_field(
            "cast",
            &serde_json::json!([{"name":"Actor","role":"Host"}])
        ));
        assert!(valid_field(
            "custom-provider-field",
            &serde_json::json!({"x":1})
        ));
    }
}

/// The revision of an item's metadata resource, from its stored source
/// revisions. Source revisions only increase, documents are never deleted, a
/// merge only adds documents to the surviving work, and the scanned origin is
/// immutable (counted as 1). The sum therefore strictly increases with every
/// change to the item's metadata inputs and never repeats, so it is a sound
/// precondition for the whole resource. `None` if it would overflow.
pub fn item_revision(source_revisions: impl IntoIterator<Item = u64>) -> Option<u64> {
    source_revisions
        .into_iter()
        .try_fold(1u64, |sum, revision| sum.checked_add(revision))
}

#[cfg(test)]
mod revision_tests {
    use super::item_revision;
    #[test]
    fn item_revision_counts_origin_and_every_source_change() {
        assert_eq!(item_revision([]), Some(1));
        assert_eq!(item_revision([1]), Some(2));
        // Advancing any one source advances the item.
        assert_eq!(item_revision([1, 3]), Some(5));
        assert_eq!(item_revision([2, 3]), Some(6));
        assert_eq!(item_revision([u64::MAX]), None);
    }
}
