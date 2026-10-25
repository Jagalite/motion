//! Lossless mapping of Motion's stored media families, not inferred genres or
//! logical classifications. Generic audio must not become a musical recording.
pub fn to_native(motion: &str) -> Option<&'static str> {
    match motion {
        "video" => Some("video"),
        "audio" => Some("custom:motion_audio"),
        _ => None,
    }
}
pub fn from_native(native: &str) -> Option<&'static str> {
    match native {
        "video" => Some("video"),
        "custom:motion_audio" => Some("audio"),
        _ => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_motion_families_round_trip_without_semantic_inference() {
        for family in ["video", "audio"] {
            assert_eq!(from_native(to_native(family).unwrap()), Some(family));
        }
        assert_eq!(to_native("audio"), Some("custom:motion_audio"));
        for unknown in ["", "recording", "movie", "custom:other", "Audio"] {
            assert_eq!(to_native(unknown), None);
            assert_eq!(from_native(unknown), None);
        }
        assert_eq!(from_native("audio"), None);
    }
}
