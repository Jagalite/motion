//! Session ordering and completion policy, independent of transport and storage.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub sequence: i64,
    pub position: f64,
    pub status: String,
}
/// None means an exact retry. Positions may move backwards when seeking;
/// ordering is established by the sequence, not by time or playback position.
pub fn advance(
    current: &Session,
    event: &Session,
    authoritative: bool,
    duration: Option<f64>,
) -> Result<Option<Session>, &'static str> {
    if !authoritative {
        return Err("superseded_session");
    }
    if event.sequence < 1
        || event.sequence == i64::MAX
        || !event.position.is_finite()
        || !(0.0..=315_360_000.0).contains(&event.position)
        || !matches!(
            event.status.as_str(),
            "playing" | "paused" | "stopped" | "ended"
        )
    {
        return Err("invalid_event");
    }
    if duration.is_some_and(|d| {
        event.position > d + 1.0 || (event.status == "ended" && event.position < (d - 1.0).max(0.0))
    }) {
        return Err("invalid_position");
    }
    if event.sequence == current.sequence && event == current {
        return Ok(None);
    }
    if matches!(
        current.status.as_str(),
        "stopped" | "ended" | "invalidated" | "superseded"
    ) {
        return Err("closed_session");
    }
    if event.sequence <= current.sequence {
        return Err("stale_sequence");
    }
    Ok(Some(event.clone()))
}
pub fn valid_language(value: &str) -> bool {
    value.len() <= 35
        && value.split('-').enumerate().all(|(i, p)| {
            !p.is_empty()
                && p.len() <= 8
                && if i == 0 {
                    p.len() >= 2 && p.bytes().all(|b| b.is_ascii_alphabetic())
                } else {
                    p.bytes().all(|b| b.is_ascii_alphanumeric())
                }
        })
}

/// Facts persisted together for a profile/item. session_id remains set after
/// invalidation so retention cannot re-enable legacy unsequenced writes.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct State {
    pub revision: i64,
    pub session_id: Option<String>,
    pub position: f64,
    pub automatic_watched: bool,
    pub manual_watched: Option<bool>,
}
impl State {
    pub fn watched(&self) -> bool {
        self.manual_watched.unwrap_or(self.automatic_watched)
    }
    pub fn override_watched(
        &self,
        expected: i64,
        watched: Option<bool>,
    ) -> Result<Self, &'static str> {
        Ok(Self {
            revision: crate::revision::advance(self.revision, expected)?,
            manual_watched: watched,
            ..self.clone()
        })
    }
    pub fn start(
        &self,
        expected: i64,
        session: String,
        duration: Option<f64>,
    ) -> Result<Self, &'static str> {
        Ok(Self {
            revision: crate::revision::advance(self.revision, expected)?,
            session_id: Some(session),
            position: duration
                .filter(|d| d.is_finite() && *d >= 0.0)
                .map(|d| self.position.min(d))
                .unwrap_or(self.position),
            ..self.clone()
        })
    }
    pub fn legacy(&self, position: f64) -> Result<Self, &'static str> {
        if self.session_id.is_some() {
            return Err("session_required");
        }
        if !position.is_finite() || !(0.0..=315_360_000.0).contains(&position) {
            return Err("invalid_position");
        }
        Ok(Self {
            position,
            revision: crate::revision::advance(self.revision, self.revision)?,
            ..self.clone()
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileIdentity {
    pub id: String,
    pub revision: String,
}
pub fn check_switch(
    current: &Session,
    file: &FileIdentity,
    event: &Session,
    selected: Option<&FileIdentity>,
) -> Result<(), &'static str> {
    if selected.is_some() && !matches!(event.status.as_str(), "playing" | "paused") {
        return Err("invalid_file_switch_status");
    }
    if event.sequence <= current.sequence && selected.is_some_and(|f| f != file) {
        return Err("stale_sequence");
    }
    Ok(())
}
/// Return the session and viewing changes as one persistence unit.
pub fn event(
    view: &State,
    session_id: &str,
    current: &Session,
    input: &Session,
    duration: Option<f64>,
) -> Result<Option<(Session, State)>, &'static str> {
    let Some(next) = advance(
        current,
        input,
        view.session_id.as_deref() == Some(session_id),
        duration,
    )?
    else {
        return Ok(None);
    };
    let revision = crate::revision::advance(view.revision, view.revision)
        .map_err(|_| "viewing_revision_exhausted")?;
    let state = State {
        revision,
        position: next.position,
        automatic_watched: view.automatic_watched || next.status == "ended",
        ..view.clone()
    };
    Ok(Some((next, state)))
}
/// File observations are checked at commit time, within the same writer boundary.
pub fn check_file(
    expected: &FileIdentity,
    observed_revision: &str,
    eligible: bool,
) -> Result<(), &'static str> {
    if expected.revision != observed_revision || !eligible {
        Err("file_revision_conflict")
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenditionBinding {
    pub item: String,
    pub output_revision: String,
    pub source_revision: String,
    pub current_source_revision: String,
}
#[derive(Clone, Debug, PartialEq)]
pub struct FileState {
    pub identity: FileIdentity,
    pub item: String,
    pub generated: bool,
    pub available: bool,
    pub duration: Option<f64>,
    pub renditions: Vec<RenditionBinding>,
}
impl FileState {
    pub fn eligible(&self, item: &str, require_available: bool) -> bool {
        (!require_available || self.available)
            && ((!self.generated && self.item == item)
                || self.renditions.iter().any(|r| {
                    r.item == item
                        && crate::renditions::available(
                            true,
                            &self.identity.revision,
                            &r.output_revision,
                            &r.current_source_revision,
                            &r.source_revision,
                        )
                }))
    }
}

/// Persist the view together with the requested closure of its previous session.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub view: State,
    pub close_session: Option<(String, &'static str)>,
}
impl State {
    pub fn override_change(
        &self,
        expected: i64,
        watched: Option<bool>,
    ) -> Result<Change, &'static str> {
        Ok(Change {
            view: self.override_watched(expected, watched)?,
            close_session: self.session_id.clone().map(|id| (id, "invalidated")),
        })
    }
    pub fn start_change(
        &self,
        expected: i64,
        id: String,
        duration: Option<f64>,
    ) -> Result<Change, &'static str> {
        Ok(Change {
            view: self.start(expected, id, duration)?,
            close_session: self.session_id.clone().map(|id| (id, "superseded")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(sequence: i64, position: f64, status: &str) -> Session {
        Session {
            sequence,
            position,
            status: status.into(),
        }
    }
    #[test]
    fn ordering_retries_seeks_and_completion() {
        let start = event(0, 0.0, "paused");
        let playing = event(1, 40.0, "playing");
        assert_eq!(
            advance(&start, &playing, true, Some(100.0)),
            Ok(Some(playing.clone()))
        );
        assert_eq!(advance(&playing, &playing, true, Some(100.0)), Ok(None));
        assert_eq!(
            advance(&playing, &playing, false, Some(100.0)),
            Err("superseded_session")
        );
        assert_eq!(
            advance(&playing, &event(1, 41.0, "playing"), true, None),
            Err("stale_sequence")
        );
        let seek = event(2, 5.0, "paused");
        assert!(advance(&playing, &seek, true, None).unwrap().is_some());
        assert_eq!(
            advance(&seek, &event(3, 5.0, "ended"), true, Some(100.0)),
            Err("invalid_position")
        );
        let ended = event(3, 100.0, "ended");
        assert!(advance(&seek, &ended, true, Some(100.0)).unwrap().is_some());
        assert_eq!(advance(&ended, &ended, true, None), Ok(None));
        assert_eq!(
            advance(&ended, &event(4, 0.0, "playing"), true, None),
            Err("closed_session")
        );
        assert_eq!(
            advance(&start, &event(1, f64::NAN, "paused"), true, None),
            Err("invalid_event")
        );
    }
    #[test]
    fn language_preferences_use_bounded_language_tags() {
        for tag in ["en", "en-US", "zh-Hant", "und"] {
            assert!(valid_language(tag));
        }
        for tag in ["", "e", "en_uk", "-en", "en--US", "en/US"] {
            assert!(!valid_language(tag));
        }
    }
}
