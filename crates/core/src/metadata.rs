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

/// Validate and normalize a contribution with the same rules the metadata
/// API applies: at most 100 fields/tags, non-empty field names, non-null and
/// registry-valid values, tags whitespace-collapsed and lower-cased (1–100
/// bytes). Excluded tags are normalized the same way.
pub fn normalize_contribution(c: &Contribution) -> Result<Contribution, &'static str> {
    if c.values.len() > 100 || c.tags.len() > 100 || c.excluded_tags.len() > 100 {
        return Err("too many fields or tags");
    }
    for (key, value) in &c.values {
        if key.is_empty() || key.len() > 100 || value.is_null() || !valid_field(key, value) {
            return Err("invalid metadata field");
        }
    }
    let tags = |values: &BTreeSet<String>| -> Result<BTreeSet<String>, &'static str> {
        values
            .iter()
            .map(|tag| {
                let tag = tag
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase();
                if tag.is_empty() || tag.len() > 100 {
                    Err("tags must contain 1-100 bytes")
                } else {
                    Ok(tag)
                }
            })
            .collect()
    };
    Ok(Contribution {
        values: c.values.clone(),
        tags: tags(&c.tags)?,
        excluded_tags: tags(&c.excluded_tags)?,
    })
}
