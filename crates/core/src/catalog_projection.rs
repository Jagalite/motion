//! Revision domains stay distinct across native migration. The original Motion
//! revision is an immutable base, not an assertion about a native owner's head.
pub fn motion_revision(base: i64, native_head: i64) -> Option<i64> {
    bridged_revision(base, 1, native_head)
}
pub fn bridged_revision(base: i64, native_base: i64, native_head: i64) -> Option<i64> {
    if base < 1 || native_base < 1 || native_head < native_base {
        return None;
    }
    base.checked_add(native_head.checked_sub(native_base)?)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_base_and_invalidates_old_tickets() {
        assert_eq!(bridged_revision(9, 4, 4), Some(9));
        assert_eq!(bridged_revision(9, 4, 5), Some(10));
        assert_eq!(bridged_revision(9, 4, 3), None);
        assert_eq!(motion_revision(9, 1), Some(9));
        assert_eq!(motion_revision(9, 2), Some(10));
        assert_eq!(motion_revision(i64::MAX, 2), None);
        assert_eq!(motion_revision(0, 1), None);
        assert_eq!(motion_revision(1, 0), None);
    }
}

/// Motion represents a version as contiguous, ordered source extents in whole
/// milliseconds. Native layouts outside that representation remain unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectedExtent {
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub next_timeline_us: i64,
}
pub fn project_extent(
    source_slice_us: Option<(i64, i64)>,
    whole_file_duration_us: Option<i64>,
    timeline_interval_us: Option<(i64, i64)>,
    expected_timeline_start_us: i64,
) -> Option<ProjectedExtent> {
    let (start_ms, end_ms, duration) = match source_slice_us {
        Some((start, end)) if start >= 0 && end > start && start % 1000 == 0 && end % 1000 == 0 => {
            (
                Some(start / 1000),
                Some(end / 1000),
                end.checked_sub(start)?,
            )
        }
        None => {
            let duration = whole_file_duration_us?;
            if duration <= 0 || duration % 1000 != 0 {
                return None;
            }
            (None, None, duration)
        }
        _ => return None,
    };
    if expected_timeline_start_us < 0 || expected_timeline_start_us % 1000 != 0 {
        return None;
    }
    let next = expected_timeline_start_us.checked_add(duration)?;
    if timeline_interval_us != Some((expected_timeline_start_us, next)) {
        return None;
    }
    Some(ProjectedExtent {
        start_ms,
        end_ms,
        next_timeline_us: next,
    })
}

#[cfg(test)]
mod extent_tests {
    use super::*;
    #[test]
    fn projects_current_slice_without_rounding_or_inventing_placement() {
        assert_eq!(
            project_extent(Some((2000, 7000)), None, Some((0, 5000)), 0),
            Some(ProjectedExtent {
                start_ms: Some(2),
                end_ms: Some(7),
                next_timeline_us: 5000
            })
        );
        assert_eq!(
            project_extent(None, Some(3000), Some((5000, 8000)), 5000),
            Some(ProjectedExtent {
                start_ms: None,
                end_ms: None,
                next_timeline_us: 8000
            })
        );
        for slice in [(-1000, 2000), (0, 0), (1, 2000), (0, 2001)] {
            assert!(project_extent(Some(slice), None, Some((0, 2000)), 0).is_none());
        }
        assert!(project_extent(None, None, None, 0).is_none());
        assert!(project_extent(None, Some(3001), Some((0, 3001)), 0).is_none());
        assert!(project_extent(None, Some(3000), Some((1000, 4000)), 0).is_none());
        assert!(project_extent(None, Some(3000), None, 0).is_none());
    }
}
