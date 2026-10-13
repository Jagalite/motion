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
    fn a_position_finds_its_thumbnail() {
        let l = layout(5_400_000, 1920, 1080).unwrap();
        assert_eq!(tile_at(&l, 0), Some((0, 0, 0)));
        assert_eq!(tile_at(&l, 19_999), Some((0, 1, 0)));
        assert_eq!(tile_at(&l, 1_000_000), Some((1, 0, 0)));
        assert_eq!(tile_at(&l, 5_399_999), Some((5, 9, 3)));
        assert_eq!(tile_at(&l, 5_400_000), None);
    }
}
