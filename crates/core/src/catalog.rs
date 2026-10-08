//! Catalog relationships are explicit; filenames never infer identity.
pub fn valid_relationship(kind: &str, parent_kind: Option<&str>, number: Option<i64>) -> bool {
    match kind {
        "movie" | "series" | "unclassified" => parent_kind.is_none() && number.is_none(),
        "season" => {
            parent_kind == Some("series") && number.is_some_and(|n| (0..=100_000).contains(&n))
        }
        "episode" => {
            parent_kind == Some("season") && number.is_some_and(|n| (0..=100_000).contains(&n))
        }
        _ => false,
    }
}

pub fn validate_structure(
    kind: &str,
    parent_kind: Option<&str>,
    number: Option<i64>,
    self_parent: bool,
    children: &[String],
    editions: i64,
) -> Result<(), &'static str> {
    if self_parent {
        return Err("An item cannot parent itself");
    }
    if !valid_relationship(kind, parent_kind, number) {
        return Err(
            "Expected series > season > episode, with nonnegative season/episode numbers; root types have no parent or number",
        );
    }
    if children.iter().any(|child| {
        !matches!(
            (kind, child.as_str()),
            ("series", "season") | ("season", "episode")
        )
    }) {
        return Err("Reclassification would invalidate existing children");
    }
    if editions > 0 && !can_own_editions(kind) {
        return Err("Series and seasons cannot own editions");
    }
    Ok(())
}
pub fn can_own_editions(kind: &str) -> bool {
    !matches!(kind, "series" | "season")
}

pub fn library_idle(active_jobs: i64) -> Result<(), &'static str> {
    if active_jobs > 0 {
        Err("library_busy")
    } else {
        Ok(())
    }
}
pub fn removable_profile(id: &str) -> bool {
    id != "default"
}

/// Revalidate the exact catalog inspected before changing a library root.
pub fn relocation<T: PartialEq>(
    verified: &[T],
    current: &[T],
    other_root_owners: i64,
) -> Result<(), &'static str> {
    if verified != current {
        Err("catalog_changed")
    } else if other_root_owners > 0 {
        Err("root_registered")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hierarchy_is_strict_and_specials_are_zero_numbered() {
        assert!(valid_relationship("season", Some("series"), Some(0)));
        assert!(valid_relationship("episode", Some("season"), Some(0)));
        for parent in ["movie", "episode", "unclassified"] {
            assert!(!valid_relationship("season", Some(parent), Some(1)));
            assert!(!valid_relationship("episode", Some(parent), Some(1)));
        }
        assert!(!valid_relationship("movie", Some("series"), None));
        assert!(!valid_relationship("episode", Some("season"), Some(-1)));
        assert!(!valid_relationship("series", None, Some(1)));
    }
}
