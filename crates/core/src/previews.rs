//! Trick-play previews: thumbnail sheets of one exact file revision. These
//! pure policies decide their layout, whether rendered sheets are what the
//! layout promises, whether a finished attempt may publish them, and where
//! a position's thumbnail is.
use serde::{Deserialize, Serialize};

/// Thumbnail width in pixels; the height follows the picture's shape.
pub const TILE_WIDTH: u32 = 160;
/// Thumbnails per sheet, row by row.
pub const COLUMNS: u32 = 10;
pub const ROWS: u32 = 10;
/// The shortest interval between thumbnails.
pub const MIN_INTERVAL_MS: u64 = 10_000;
/// At most this many thumbnails (20 sheets); longer titles space them out.
pub const MAX_TILES: u64 = 2_000;
/// A rendered sheet larger than this is refused.
pub const MAX_SHEET_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    /// Thumbnail `n` (from 0) shows the picture at `n * interval_ms`.
    pub interval_ms: u64,
    pub tile_width: u32,
    pub tile_height: u32,
    pub columns: u32,
    pub rows: u32,
    pub tiles: u32,
    pub sheets: u32,
}

/// The layout for a file of `duration_ms` whose pictures are `width` x
/// `height` (display shape): None without a usable duration or picture.
pub fn layout(duration_ms: u64, width: u32, height: u32) -> Option<Layout> {
    if duration_ms == 0 || width == 0 || height == 0 {
        return None;
    }
    let interval_ms = MIN_INTERVAL_MS.max(duration_ms.div_ceil(MAX_TILES));
    let tiles = duration_ms.div_ceil(interval_ms);
    // An even height for the encoder, at least 2 pixels.
    let tile_height = ((u64::from(TILE_WIDTH) * u64::from(height) / u64::from(width)) as u32 & !1)
        .clamp(2, TILE_WIDTH * 4);
    let per_sheet = COLUMNS * ROWS;
    let tiles = u32::try_from(tiles).ok()?;
    Some(Layout {
        interval_ms,
        tile_width: TILE_WIDTH,
        tile_height,
        columns: COLUMNS,
        rows: ROWS,
        tiles,
        sheets: tiles.div_ceil(per_sheet),
    })
}

/// Facts observed atomically by every preview admission adapter.
#[derive(Clone, Copy, Debug)]
pub struct AdmissionFacts {
    pub source_current: bool,
    pub requester_authorized: bool,
    pub layout_valid: bool,
    pub equivalent_work: bool,
    pub active_jobs: u64,
    pub capacity: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Enqueue,
    Existing,
    Invalid,
    Full,
}
impl AdmissionFacts {
    pub fn decide(self) -> Admission {
        if !self.source_current || !self.requester_authorized || !self.layout_valid {
            Admission::Invalid
        } else if self.equivalent_work {
            Admission::Existing
        } else if self.active_jobs >= self.capacity {
            Admission::Full
        } else {
            Admission::Enqueue
        }
    }
}

/// A rendered sheet as observed: its decoded size and its bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sheet {
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
}

/// Whether rendered sheets are exactly what `layout` promises: as many
/// sheets as the layout has (a renderer may stop a sheet short at the end,
/// so the last may hold fewer tiles but has the full grid), each a full grid
/// of tiles, and none oversized.
pub fn valid_sheets(layout: &Layout, sheets: &[Sheet]) -> bool {
    sheets.len() == layout.sheets as usize
        && sheets.iter().all(|s| {
            s.width == layout.tile_width * layout.columns
                && s.height == layout.tile_height * layout.rows
                && s.bytes > 0
                && s.bytes <= MAX_SHEET_BYTES
        })
}

/// A rendered thumbnail larger than this is refused.
pub const MAX_THUMBNAIL_BYTES: u64 = 256 * 1024;

/// Whether rendered thumbnails are exactly the layout's: one per tile (the
/// picture at each `n * interval_ms`, so a sheet never shows a blank tile
/// the manifest promises), each the tile size and not oversized.
pub fn valid_thumbnails(layout: &Layout, thumbnails: &[Sheet]) -> bool {
    thumbnails.len() == layout.tiles as usize
        && thumbnails.iter().all(|t| {
            t.width == layout.tile_width
                && t.height == layout.tile_height
                && t.bytes > 0
                && t.bytes <= MAX_THUMBNAIL_BYTES
        })
}

/// What a finished preview attempt must hold to publish its sheets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicationFacts {
    /// The job is still running this attempt (not cancelled or superseded).
    pub attempt_current: bool,
    /// The file is still available at exactly the revision rendered.
    pub source_current: bool,
    /// The sheets are what the layout promises (`valid_sheets`), and the
    /// source did not change while they were rendered.
    pub sheets_valid: bool,
    /// Whoever asked may still request processing of that file.
    pub requester_authorized: bool,
}

impl PublicationFacts {
    pub fn publishable(self) -> bool {
        self.attempt_current
            && self.source_current
            && self.sheets_valid
            && self.requester_authorized
    }
}

/// Identity of the artifact a worker rendered or previously published.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactIdentity {
    pub file: String,
    pub revision: String,
    pub layout_identity: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublicationAction {
    /// An equivalent row already exists; no replacement is necessary.
    Reuse { artifact: ArtifactIdentity },
    /// Sync and rename this attempt's staging directory, then persist the row
    /// in the same reserved publication transaction.
    Publish {
        job: String,
        attempt: u32,
        artifact: ArtifactIdentity,
        layout: Layout,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationPlan {
    /// None means an obsolete worker must not update the current row.
    pub job: Option<crate::jobs::Job>,
    pub error: Option<String>,
    pub action: Option<PublicationAction>,
    pub remove_staging: bool,
}

/// Decide attempt completion and exact publication identity from observations.
/// Filesystem sync/rename precede row commit; cleanup happens after commit.
/// An obsolete worker never rewrites the current job or its error metadata.
pub fn plan_publication(
    current: &crate::jobs::Job,
    attempt: u32,
    job_id: &str,
    artifact: &ArtifactIdentity,
    current_layout_identity: Option<&str>,
    rendered: Option<(&Layout, &[Sheet], bool)>,
    requester_authorized: bool,
    previous: Option<&ArtifactIdentity>,
    stuck: bool,
) -> PublicationPlan {
    if current.attempt != attempt
        || !matches!(
            current.phase,
            crate::jobs::Phase::Running | crate::jobs::Phase::Cancelling
        )
    {
        return PublicationPlan {
            job: None,
            error: None,
            action: None,
            remove_staging: !stuck,
        };
    }
    let valid = current.executing(attempt)
        && requester_authorized
        && current_layout_identity == Some(artifact.layout_identity.as_str())
        && rendered
            .is_some_and(|(layout, sheets, unchanged)| unchanged && valid_sheets(layout, sheets));
    let (job, _) = crate::jobs::transition(
        current,
        crate::jobs::Input::Finished {
            attempt,
            success: valid,
        },
    );
    let action = if valid {
        if previous == Some(artifact) {
            Some(PublicationAction::Reuse {
                artifact: artifact.clone(),
            })
        } else {
            Some(PublicationAction::Publish {
                job: job_id.to_owned(),
                attempt,
                artifact: artifact.clone(),
                layout: *rendered.unwrap().0,
            })
        }
    } else {
        None
    };
    let error = (job.phase == crate::jobs::Phase::Failed).then(|| "preview_failed".to_owned());
    PublicationPlan {
        job: Some(job),
        error,
        action,
        remove_staging: !stuck,
    }
}

/// A published row and the current layout/sheet observations needed to decide
/// whether it can still be served. The adapter bounds sheet observations to 20.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionObservation {
    pub artifact: ArtifactIdentity,
    pub current_layout_identity: Option<String>,
    pub sheet_count: i64,
    pub sheets_present: Vec<bool>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetentionAction {
    Keep,
    Withdraw { artifact: ArtifactIdentity },
}
pub fn retain_publication(observed: &RetentionObservation) -> RetentionAction {
    let valid = observed.current_layout_identity.as_deref()
        == Some(observed.artifact.layout_identity.as_str())
        && (1..=20).contains(&observed.sheet_count)
        && observed.sheets_present.len() == observed.sheet_count as usize
        && observed.sheets_present.iter().all(|present| *present);
    if valid {
        RetentionAction::Keep
    } else {
        RetentionAction::Withdraw {
            artifact: observed.artifact.clone(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CleanupAction {
    Keep,
    RemoveOrphan { file: String, revision: String },
    RemoveStaging { job: String, attempt: i64 },
}
/// The exact orphan is removable only when no committed row owns it.
pub fn orphan_cleanup(file: &str, revision: &str, row_present: bool) -> CleanupAction {
    if row_present {
        CleanupAction::Keep
    } else {
        CleanupAction::RemoveOrphan {
            file: file.to_owned(),
            revision: revision.to_owned(),
        }
    }
}
/// A staging directory remains while its exact attempt is active or a worker
/// witness is still held. A malformed attempt uses the adapter's -1 sentinel.
pub fn staging_cleanup(
    job_id: &str,
    job: Option<(crate::jobs::Phase, i64)>,
    directory_attempt: i64,
    witness_held: bool,
) -> CleanupAction {
    if witness_held
        || job.is_some_and(|(phase, attempt)| {
            attempt == directory_attempt
                && matches!(
                    phase,
                    crate::jobs::Phase::Running | crate::jobs::Phase::Cancelling
                )
        })
    {
        CleanupAction::Keep
    } else {
        CleanupAction::RemoveStaging {
            job: job_id.to_owned(),
            attempt: directory_attempt,
        }
    }
}

/// Where the thumbnail for `position_ms` is: (sheet, column, row), the
/// thumbnail at or before the position. None beyond the last one.
pub fn tile_at(layout: &Layout, position_ms: u64) -> Option<(u32, u32, u32)> {
    let index = u32::try_from(position_ms / layout.interval_ms).ok()?;
    if index >= layout.tiles {
        return None;
    }
    let per_sheet = layout.columns * layout.rows;
    let within = index % per_sheet;
    Some((
        index / per_sheet,
        within % layout.columns,
        within / layout.columns,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_checks_authority_and_identity_before_deduplication_and_capacity() {
        for current in [false, true] {
            for authorized in [false, true] {
                for valid in [false, true] {
                    for existing in [false, true] {
                        for active in [0, 99, 100, 101] {
                            let facts = AdmissionFacts {
                                source_current: current,
                                requester_authorized: authorized,
                                layout_valid: valid,
                                equivalent_work: existing,
                                active_jobs: active,
                                capacity: 100,
                            };
                            let expected = if !current || !authorized || !valid {
                                Admission::Invalid
                            } else if existing {
                                Admission::Existing
                            } else if active >= 100 {
                                Admission::Full
                            } else {
                                Admission::Enqueue
                            };
                            assert_eq!(facts.decide(), expected);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn layouts_bound_tiles_and_follow_the_picture_shape() {
        // A 90-minute 16:9 title: one thumbnail every 10 s.
        let l = layout(5_400_000, 1920, 1080).unwrap();
        assert_eq!((l.interval_ms, l.tiles, l.sheets), (10_000, 540, 6));
        assert_eq!((l.tile_width, l.tile_height), (160, 90));
        // A very long title spaces them out to at most MAX_TILES.
        let l = layout(30 * 3_600_000, 1920, 1080).unwrap();
        assert!(u64::from(l.tiles) <= MAX_TILES && l.interval_ms > MIN_INTERVAL_MS);
        // A short one still has one thumbnail; odd heights become even.
        let l = layout(3_000, 720, 405).unwrap();
        assert_eq!((l.tiles, l.sheets, l.tile_height), (1, 1, 90));
        assert_eq!(layout(0, 1920, 1080), None);
        assert_eq!(layout(1_000, 0, 1080), None);
    }

    #[test]
    fn only_the_promised_sheets_validate_and_publish() {
        let l = layout(250_000, 1920, 1080).unwrap();
        assert_eq!(l.sheets, 1);
        let good = Sheet {
            width: 1600,
            height: 900,
            bytes: 50_000,
        };
        assert!(valid_sheets(&l, &[good]));
        assert!(!valid_sheets(&l, &[]));
        assert!(!valid_sheets(&l, &[good, good]));
        assert!(!valid_sheets(
            &l,
            &[Sheet {
                width: 1280,
                ..good
            }]
        ));
        assert!(!valid_sheets(
            &l,
            &[Sheet {
                bytes: MAX_SHEET_BYTES + 1,
                ..good
            }]
        ));
        assert!(!valid_sheets(&l, &[Sheet { bytes: 0, ..good }]));
        let facts = PublicationFacts {
            attempt_current: true,
            source_current: true,
            sheets_valid: true,
            requester_authorized: true,
        };
        assert!(facts.publishable());
        for broken in [
            PublicationFacts {
                attempt_current: false,
                ..facts
            },
            PublicationFacts {
                source_current: false,
                ..facts
            },
            PublicationFacts {
                sheets_valid: false,
                ..facts
            },
            PublicationFacts {
                requester_authorized: false,
                ..facts
            },
        ] {
            assert!(!broken.publishable());
        }
    }

    #[test]
    fn exactly_one_thumbnail_per_tile_validates() {
        let l = layout(25_000, 320, 240).unwrap();
        let thumb = Sheet {
            width: 160,
            height: 120,
            bytes: 4_000,
        };
        assert!(valid_thumbnails(&l, &[thumb; 3]));
        assert!(!valid_thumbnails(&l, &[thumb; 2]));
        assert!(!valid_thumbnails(&l, &[thumb; 4]));
        assert!(!valid_thumbnails(
            &l,
            &[Sheet {
                height: 90,
                ..thumb
            }; 3]
        ));
        assert!(!valid_thumbnails(
            &l,
            &[Sheet {
                bytes: MAX_THUMBNAIL_BYTES + 1,
                ..thumb
            }; 3]
        ));
    }

    #[test]
    fn a_position_finds_its_thumbnail() {
        let l = layout(5_400_000, 1920, 1080).unwrap();
        assert_eq!(tile_at(&l, 0), Some((0, 0, 0)));
        assert_eq!(tile_at(&l, 19_999), Some((0, 1, 0)));
        assert_eq!(tile_at(&l, 1_000_000), Some((1, 0, 0)));
        assert_eq!(tile_at(&l, 5_399_999), Some((5, 9, 3)));
        assert_eq!(tile_at(&l, 5_400_000), None);
    }
}

#[cfg(test)]
mod publication_plan_tests {
    use super::*;
    use crate::jobs::{Job, Phase};
    #[test]
    fn publication_fences_identity_and_selects_exact_action() {
        let layout = layout(10_000, 160, 90).unwrap();
        let sheets = vec![Sheet {
            width: layout.tile_width * layout.columns,
            height: layout.tile_height * layout.rows,
            bytes: 10,
        }];
        let artifact = ArtifactIdentity {
            file: "f".into(),
            revision: "r".into(),
            layout_identity: "l".into(),
        };
        let job = Job {
            phase: Phase::Running,
            attempt: 2,
        };
        let plan = |attempt, identity, previous: Option<&ArtifactIdentity>, stuck| {
            plan_publication(
                &job,
                attempt,
                "job",
                &artifact,
                identity,
                Some((&layout, &sheets, true)),
                true,
                previous,
                stuck,
            )
        };
        let new = plan(2, Some("l"), None, false);
        assert_eq!(new.job.as_ref().unwrap().phase, Phase::Completed);
        assert_eq!(
            new.action,
            Some(PublicationAction::Publish {
                job: "job".into(),
                attempt: 2,
                artifact: artifact.clone(),
                layout
            })
        );
        assert!(new.remove_staging);
        assert_eq!(
            plan(2, Some("l"), Some(&artifact), false).action,
            Some(PublicationAction::Reuse {
                artifact: artifact.clone()
            })
        );
        assert_eq!(
            plan(2, Some("other"), None, false).job.unwrap().phase,
            Phase::Failed
        );
        assert!(!plan(2, Some("l"), None, true).remove_staging);
        assert_eq!(plan(1, Some("l"), None, false).job, None);
        let cancelling = Job {
            phase: Phase::Cancelling,
            attempt: 2,
        };
        let cancelled = plan_publication(
            &cancelling,
            2,
            "job",
            &artifact,
            Some("l"),
            Some((&layout, &sheets, true)),
            true,
            None,
            false,
        );
        assert_eq!(cancelled.job.unwrap().phase, Phase::Cancelled);
        assert_eq!(cancelled.action, None);
    }
}

#[cfg(test)]
mod retention_plan_tests {
    use super::*;
    use crate::jobs::Phase;
    #[test]
    fn withdrawal_and_cleanup_require_current_evidence() {
        let artifact = ArtifactIdentity {
            file: "f".into(),
            revision: "r".into(),
            layout_identity: "layout".into(),
        };
        let mut observed = RetentionObservation {
            artifact: artifact.clone(),
            current_layout_identity: Some("layout".into()),
            sheet_count: 2,
            sheets_present: vec![true, true],
        };
        assert_eq!(retain_publication(&observed), RetentionAction::Keep);
        observed.sheets_present[1] = false;
        assert_eq!(
            retain_publication(&observed),
            RetentionAction::Withdraw {
                artifact: artifact.clone()
            }
        );
        observed.sheets_present[1] = true;
        observed.current_layout_identity = Some("changed".into());
        assert_eq!(
            retain_publication(&observed),
            RetentionAction::Withdraw {
                artifact: artifact.clone()
            }
        );
        observed.current_layout_identity = None;
        assert_eq!(
            retain_publication(&observed),
            RetentionAction::Withdraw { artifact }
        );
        assert_eq!(orphan_cleanup("f", "r", true), CleanupAction::Keep);
        assert_eq!(
            orphan_cleanup("f", "r", false),
            CleanupAction::RemoveOrphan {
                file: "f".into(),
                revision: "r".into()
            }
        );
        assert_eq!(
            staging_cleanup("j", Some((Phase::Running, 2)), 2, false),
            CleanupAction::Keep
        );
        assert_eq!(
            staging_cleanup("j", Some((Phase::Cancelling, 2)), 2, false),
            CleanupAction::Keep
        );
        assert_eq!(
            staging_cleanup("j", Some((Phase::Completed, 2)), 2, true),
            CleanupAction::Keep
        );
        assert_eq!(
            staging_cleanup("j", Some((Phase::Running, 3)), 2, false),
            CleanupAction::RemoveStaging {
                job: "j".into(),
                attempt: 2
            }
        );
        assert_eq!(
            staging_cleanup("j", None, -1, false),
            CleanupAction::RemoveStaging {
                job: "j".into(),
                attempt: -1
            }
        );
    }
}
