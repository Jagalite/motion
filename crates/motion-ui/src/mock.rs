//! Development/test double for `UiQueryFacade`.
//!
//! NOT a Motion server read model. It reports `is_mock() == true`, every page
//! rendered from it carries a visible banner, and passing against it is not
//! evidence of backend behaviour (plan section 19.2, Wave 1). It does model the
//! one rule pages must respect: titles restricted for a profile are invisible,
//! indistinguishable from missing.

use crate::facade::*;

struct Title {
    id: &'static str,
    title: &'static str,
    kind: &'static str,
    library: &'static str,
    parent: Option<&'static str>,
    timeline: Option<&'static str>,
    duration_ms: u64,
    restricted: bool,
    needs_review: bool,
}

const TITLES: &[Title] = &[
    Title {
        id: "item1",
        title: "Arrival of the Night Train",
        kind: "movie",
        library: "lib1",
        parent: None,
        timeline: Some("tl1"),
        duration_ms: 5_400_000,
        restricted: false,
        needs_review: false,
    },
    Title {
        id: "item2",
        title: "Harbour Lights",
        kind: "movie",
        library: "lib1",
        parent: None,
        timeline: Some("tl2"),
        duration_ms: 6_300_000,
        restricted: false,
        needs_review: true,
    },
    Title {
        id: "item3",
        title: "Midnight Ledger",
        kind: "movie",
        library: "lib1",
        parent: None,
        timeline: Some("tl3"),
        duration_ms: 7_200_000,
        restricted: true,
        needs_review: false,
    },
    Title {
        id: "item4",
        title: "The Lighthouse Keepers",
        kind: "series",
        library: "lib2",
        parent: None,
        timeline: None,
        duration_ms: 0,
        restricted: false,
        needs_review: false,
    },
    Title {
        id: "item5",
        title: "Season 1",
        kind: "season",
        library: "lib2",
        parent: Some("item4"),
        timeline: None,
        duration_ms: 0,
        restricted: false,
        needs_review: false,
    },
    Title {
        id: "item6",
        title: "Pilot",
        kind: "episode",
        library: "lib2",
        parent: Some("item5"),
        timeline: Some("tl6"),
        duration_ms: 2_700_000,
        restricted: false,
        needs_review: false,
    },
    Title {
        id: "item7",
        title: "The Second Lamp",
        kind: "episode",
        library: "lib2",
        parent: Some("item5"),
        timeline: Some("tl7"),
        duration_ms: 2_640_000,
        restricted: false,
        needs_review: false,
    },
];

/// Per-profile resume positions for the mock.
const PROGRESS: &[(&str, &str, u64)] =
    &[("everyone", "tl1", 1_830_000), ("everyone", "tl6", 600_000)];

pub struct MockUiQueryFacade;

fn visible(who: &UiPrincipal, title: &Title) -> bool {
    who.can("catalog:read") && !(title.restricted && who.profile_id == "kids")
}

fn card(title: &Title) -> ItemCard {
    ItemCard {
        id: title.id.into(),
        title: title.title.into(),
        kind: title.kind.into(),
        availability: Availability::Available,
        needs_review: title.needs_review,
    }
}

fn libraries() -> Vec<LibraryCard> {
    vec![
        LibraryCard {
            id: "lib1".into(),
            name: "Films".into(),
            kind: "movies".into(),
            availability: Availability::Available,
        },
        LibraryCard {
            id: "lib2".into(),
            name: "Television".into(),
            kind: "television".into(),
            availability: Availability::Degraded,
        },
    ]
}

fn progress(who: &UiPrincipal, timeline: &str) -> u64 {
    PROGRESS
        .iter()
        .find(|(p, t, _)| *p == who.profile_id && *t == timeline)
        .map(|(_, _, ms)| *ms)
        .unwrap_or(0)
}

fn find<'t>(who: &UiPrincipal, id: &str) -> UiResult<&'t Title> {
    TITLES
        .iter()
        .find(|t| t.id == id && visible(who, t))
        .ok_or(UiError::NotFound)
}

impl UiQueryFacade for MockUiQueryFacade {
    fn is_mock(&self) -> bool {
        true
    }

    fn home<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<HomeView>> {
        Box::pin(async move {
            if !who.can("catalog:read") {
                return Err(UiError::Denied);
            }
            let continue_watching = TITLES
                .iter()
                .filter(|t| visible(who, t))
                .filter_map(|t| {
                    let timeline = t.timeline?;
                    let position = progress(who, timeline);
                    (position > 0).then(|| ContinueCard {
                        item: card(t),
                        timeline_id: timeline.into(),
                        position_ms: position,
                        duration_ms: Some(t.duration_ms),
                    })
                })
                .collect();
            Ok(HomeView {
                continue_available: true,
                continue_watching,
                libraries: libraries(),
            })
        })
    }

    fn library<'a>(
        &'a self,
        who: &'a UiPrincipal,
        id: &'a str,
    ) -> BoxFuture<'a, UiResult<LibraryView>> {
        Box::pin(async move {
            if !who.can("catalog:read") {
                return Err(UiError::Denied);
            }
            let library = libraries()
                .into_iter()
                .find(|l| l.id == id)
                .ok_or(UiError::NotFound)?;
            let items = TITLES
                .iter()
                .filter(|t| t.library == id && t.parent.is_none() && visible(who, t))
                .map(card)
                .collect();
            Ok(LibraryView { library, items })
        })
    }

    fn item<'a>(&'a self, who: &'a UiPrincipal, id: &'a str) -> BoxFuture<'a, UiResult<ItemView>> {
        Box::pin(async move {
            let title = find(who, id)?;
            let children = TITLES
                .iter()
                .filter(|t| t.parent == Some(title.id) && visible(who, t))
                .map(card)
                .collect();
            let timelines = title
                .timeline
                .map(|timeline| {
                    let position = progress(who, timeline);
                    vec![TimelineView {
                        id: timeline.into(),
                        edition: "Theatrical".into(),
                        duration_ms: Some(title.duration_ms),
                        viewing: Some(TimelineViewing { position_ms: position, watched: false }),
                        versions: vec![VersionView {
                            id: format!("ver-{timeline}"),
                            label: "1080p original".into(),
                            availability: Availability::Available,
                            summary: "H.264 1920×1080 · audio: English 2ch, Français 2ch · subtitles: English SDH".into(),
                        }],
                    }]
                })
                .unwrap_or_default();
            Ok(ItemView {
                playback_available: true,
                item: card(title),
                children,
                timelines,
            })
        })
    }

    fn search<'a>(
        &'a self,
        who: &'a UiPrincipal,
        query: &'a str,
    ) -> BoxFuture<'a, UiResult<SearchView>> {
        Box::pin(async move {
            if !who.can("catalog:read") {
                return Err(UiError::Denied);
            }
            let needle = query.trim().to_lowercase();
            let items = if needle.is_empty() {
                Vec::new()
            } else {
                TITLES
                    .iter()
                    .filter(|t| visible(who, t) && t.title.to_lowercase().contains(&needle))
                    .map(card)
                    .collect()
            };
            Ok(SearchView {
                query: query.trim().into(),
                items,
            })
        })
    }

    fn player<'a>(
        &'a self,
        who: &'a UiPrincipal,
        timeline_id: &'a str,
    ) -> BoxFuture<'a, UiResult<PlayerView>> {
        Box::pin(async move {
            if !who.can("playback:request") {
                return Err(UiError::Denied);
            }
            let title = TITLES
                .iter()
                .find(|t| t.timeline == Some(timeline_id) && visible(who, t))
                .ok_or(UiError::NotFound)?;
            Ok(PlayerView {
                item_id: title.id.into(),
                title: title.title.into(),
                timeline_id: timeline_id.into(),
                duration_ms: Some(title.duration_ms),
                resume_ms: progress(who, timeline_id),
                viewing_revision: "0".into(),
                viewing_manual_epoch: "0".into(),
                audio_tracks: vec![],
                audio_version: None,
                audio_file: None,
            })
        })
    }

    fn profiles<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<ProfilesView>> {
        Box::pin(async move {
            Ok(ProfilesView {
                profiles: who.profiles.clone(),
                current: who.profile_id.clone(),
                preferences: PreferencesView {
                    audio_languages: vec!["en".into()],
                    subtitle_languages: vec!["en".into()],
                    subtitle_mode: "foreign_audio".into(),
                    quality_mode: "auto".into(),
                    allow_client_software_decode: true,
                    autoplay: false,
                    completion_percent: 90.0,
                    etag: format!("\"prefs-{}-1\"", who.profile_id),
                },
            })
        })
    }

    fn sources<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<SourcesView>> {
        Box::pin(async move {
            if !who.can("sources:manage") {
                return Err(UiError::Denied);
            }
            Ok(SourcesView {
                scans_available: true,
                sources: vec![SourceRow {
                    id: "src1".into(),
                    name: "Movies disk".into(),
                    root_path: "/media/movies".into(),
                    availability: Availability::Available,
                }],
                libraries: libraries(),
                scans: vec![ScanRow {
                    id: "scan1".into(),
                    library_name: "Television".into(),
                    status: "partial".into(),
                    started: "2026-10-09 12:00".into(),
                    sources: vec![ScanSourceRow {
                        source_name: "Movies disk".into(),
                        status: "partial".into(),
                        observed_files: 14,
                        complete_directories: 3,
                        incomplete_directories: 1,
                        error_codes: vec!["permission_denied".into()],
                    }],
                }],
            })
        })
    }

    fn matches<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<Vec<MatchRow>>> {
        Box::pin(async move {
            if !who.can("catalog:write") {
                return Err(UiError::Denied);
            }
            Ok(vec![
                MatchRow {
                    id: "m2".into(),
                    subject: "Untitled home video".into(),
                    status: "review".into(),
                    etag: "\"r4\"".into(),
                    candidates: vec![],
                },
                MatchRow {
                    id: "m1".into(),
                    subject: "Harbour Lights".into(),
                    status: "review".into(),
                    etag: "\"r1\"".into(),
                    candidates: vec![
                        MatchCandidateView {
                            id: "mc1".into(),
                            title: "Harbour Lights (1998)".into(),
                            confidence_percent: Some(62),
                            reasons: vec!["title".into(), "year".into()],
                        },
                        MatchCandidateView {
                            id: "mc2".into(),
                            title: "Harbour Lights (2017)".into(),
                            confidence_percent: Some(55),
                            reasons: vec!["title".into()],
                        },
                    ],
                },
            ])
        })
    }

    fn jobs<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<Vec<JobRow>>> {
        Box::pin(async move {
            if !who.can("processing:request") {
                return Err(UiError::Denied);
            }
            Ok(vec![
                JobRow {
                    id: "job1".into(),
                    kind: "probe".into(),
                    phase: "completed".into(),
                    progress_percent: Some(100),
                    error_code: None,
                },
                JobRow {
                    id: "job2".into(),
                    kind: "rendition".into(),
                    phase: "running".into(),
                    progress_percent: Some(40),
                    error_code: None,
                },
            ])
        })
    }

    fn diagnostics<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<DiagnosticsView>> {
        Box::pin(async move {
            if !who.can("system:admin") {
                return Err(UiError::Denied);
            }
            Ok(DiagnosticsView {
                server_id: "mock-server".into(),
                server_version: "motion-mock".into(),
                api_version: "2.0.0".into(),
                schema_version: "0".into(),
                health: "ok".into(),
                uptime_seconds: 3600,
                active_deliveries: 0,
                queued_jobs: 0,
                running_jobs: 1,
                worker_errors: vec![],
            })
        })
    }
}
