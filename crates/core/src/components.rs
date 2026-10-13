//! Media components: logical audio/subtitle roles of a timeline and their
//! revision-bound occurrences in each version's files.
//!
//! A component is keyed by kind, normalized language, role flags and
//! normalized title, never by language alone; when one file carries several
//! tracks with the same key they stay distinct components (ordinal). Each
//! occurrence pins the exact file revision and embedded track index or
//! sidecar path. Component IDs are derived deterministically from the
//! timeline and key, so they are stable across rescans without storing a
//! second copy of what probes already record.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub type Id = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Audio,
    Subtitle,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Roles {
    pub forced: bool,
    pub hearing_impaired: bool,
    pub commentary: bool,
}

/// One observed track: embedded (`track_index`) or a sidecar file (`sidecar`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observed {
    pub file_id: Id,
    pub file_revision: String,
    pub kind: Kind,
    pub language: Option<String>,
    pub title: Option<String>,
    pub roles: Roles,
    pub default: bool,
    pub track_index: Option<i64>,
    pub sidecar: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    pub version_id: Id,
    pub file_id: Id,
    pub file_revision: String,
    pub track_index: Option<i64>,
    pub sidecar: Option<String>,
    pub default: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
    pub id: Id,
    pub kind: Kind,
    pub language: Option<String>,
    pub title: Option<String>,
    pub roles: Roles,
    pub ordinal: u32,
    pub occurrences: Vec<Occurrence>,
}

/// Resolve an explicit component only on the chosen revision-bound file.
/// Sidecars and occurrences on another cut/file never substitute for this pin.
pub fn embedded_track(
    components: &[Component],
    id: &str,
    kind: Kind,
    version: &str,
    file: &str,
    revision: &str,
) -> Option<i64> {
    components
        .iter()
        .find(|c| c.id == id && c.kind == kind)?
        .occurrences
        .iter()
        .find(|o| {
            o.version_id == version
                && o.file_id == file
                && o.file_revision == revision
                && o.sidecar.is_none()
        })?
        .track_index
}

/// The component of `kind` that the embedded track `track_index` of one
/// revision-bound file of `version` is an occurrence of.
pub fn component_of<'a>(
    components: &'a [Component],
    kind: Kind,
    version: &str,
    file: &str,
    revision: &str,
    track_index: i64,
) -> Option<&'a Component> {
    components.iter().find(|c| {
        c.kind == kind
            && c.occurrences.iter().any(|o| {
                o.version_id == version
                    && o.file_id == file
                    && o.file_revision == revision
                    && o.sidecar.is_none()
                    && o.track_index == Some(track_index)
            })
    })
}

/// The embedded track of `component` on each of `files` (file, revision) of
/// `version`, in order: one exact occurrence per file, so a multipart version
/// plays the same audio in every part even when the parts order their tracks
/// differently. None when any file has no occurrence or more than one.
pub fn per_file_tracks(
    component: &Component,
    version: &str,
    files: &[(&str, &str)],
) -> Option<Vec<i64>> {
    files
        .iter()
        .map(|(file, revision)| {
            let mut found = component.occurrences.iter().filter(|o| {
                o.version_id == version
                    && o.file_id == *file
                    && o.file_revision == *revision
                    && o.sidecar.is_none()
            });
            let o = found.next()?;
            if found.next().is_some() {
                return None;
            }
            o.track_index
        })
        .collect()
}

/// Where an explicit subtitle component occurs on one revision-bound file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubtitleOccurrence {
    /// Embedded stream index (as probed).
    Embedded(i64),
    /// Sidecar file name next to the media file.
    Sidecar(String),
}

/// Resolve an explicit subtitle component only on the chosen revision-bound
/// file: its embedded track there, or its sidecar there. Occurrences on
/// another version, file or revision never substitute.
pub fn subtitle_occurrence(
    components: &[Component],
    id: &str,
    version: &str,
    file: &str,
    revision: &str,
) -> Option<SubtitleOccurrence> {
    let mut found = components
        .iter()
        .find(|c| c.id == id && c.kind == Kind::Subtitle)?
        .occurrences
        .iter()
        .filter(|o| o.version_id == version && o.file_id == file && o.file_revision == revision);
    let o = found.next()?;
    // One component has at most one occurrence per file; more is ambiguous.
    if found.next().is_some() {
        return None;
    }
    match (&o.sidecar, o.track_index) {
        (Some(name), None) => Some(SubtitleOccurrence::Sidecar(name.clone())),
        (None, Some(index)) => Some(SubtitleOccurrence::Embedded(index)),
        _ => None,
    }
}

/// Whether two language tags name the same language: equal primary subtags,
/// with common ISO 639-2 (bibliographic and terminology) codes mapped to
/// their ISO 639-1 form. Unknown languages never match.
pub fn same_language(a: &str, b: &str) -> bool {
    fn primary(tag: &str) -> Option<String> {
        let tag = normalize_language(Some(tag))?;
        let primary = tag.split('-').next()?.to_owned();
        Some(
            match primary.as_str() {
                "eng" => "en",
                "fre" | "fra" => "fr",
                "ger" | "deu" => "de",
                "spa" => "es",
                "ita" => "it",
                "jpn" => "ja",
                "kor" => "ko",
                "chi" | "zho" => "zh",
                "por" => "pt",
                "rus" => "ru",
                "dut" | "nld" => "nl",
                "swe" => "sv",
                "nor" => "no",
                "dan" => "da",
                "fin" => "fi",
                "pol" => "pl",
                "tur" => "tr",
                "ara" => "ar",
                "heb" => "he",
                "hin" => "hi",
                "gre" | "ell" => "el",
                "cze" | "ces" => "cs",
                "hun" => "hu",
                "tha" => "th",
                "ukr" => "uk",
                "vie" => "vi",
                other => other,
            }
            .to_owned(),
        )
    }
    primary(a).is_some_and(|a| primary(b).is_some_and(|b| a == b))
}

/// The profile's subtitle preference (`Preferences::subtitle_mode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubtitleMode {
    Off,
    /// Forced (foreign-parts) subtitles in the audio's language only.
    Forced,
    /// Full subtitles in a preferred language whenever available.
    Always,
    /// Full subtitles when the audio is in a language the profile does not
    /// list; otherwise forced subtitles in the audio's language.
    ForeignAudio,
}

/// One subtitle available on the planned file, for automatic selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubtitleOption {
    pub language: Option<String>,
    pub roles: Roles,
    /// This client can be given it on some route (text it presents, or a
    /// bitmap it accepts burned in).
    pub deliverable: bool,
}

/// The outcome of automatic subtitle selection, by index into the options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomaticSubtitle {
    None,
    Selected(usize),
    /// `require` found no deliverable subtitle in a preferred language.
    Unavailable,
    /// `require` found several equally suitable ones; the client must choose.
    Ambiguous,
}

/// Choose a subtitle without an explicit pin. `require` asks for full
/// subtitles in the profile's preferred languages (in order) and fails rather
/// than substitute another language or the first track; `auto` follows the
/// profile's mode and plays without subtitles when nothing (or nothing
/// unambiguous) matches. Commentary and undeliverable tracks are never chosen
/// automatically. Among equally preferred full subtitles, plain ones rank
/// above SDH; a remaining tie is ambiguous.
pub fn automatic_subtitle(
    require: bool,
    mode: SubtitleMode,
    audio_language: Option<&str>,
    audio_languages: &[String],
    subtitle_languages: &[String],
    options: &[SubtitleOption],
) -> AutomaticSubtitle {
    let eligible = |o: &SubtitleOption| o.deliverable && !o.roles.commentary;
    let full = || -> Result<Option<usize>, ()> {
        for language in subtitle_languages {
            let matching: Vec<usize> = (0..options.len())
                .filter(|&i| {
                    let o = &options[i];
                    eligible(o)
                        && !o.roles.forced
                        && o.language
                            .as_deref()
                            .is_some_and(|l| same_language(l, language))
                })
                .collect();
            if matching.is_empty() {
                continue;
            }
            let plain: Vec<usize> = matching
                .iter()
                .copied()
                .filter(|&i| !options[i].roles.hearing_impaired)
                .collect();
            let best = if plain.is_empty() { matching } else { plain };
            return if best.len() == 1 {
                Ok(Some(best[0]))
            } else {
                Err(())
            };
        }
        Ok(None)
    };
    let forced = || -> Option<usize> {
        let audio = audio_language?;
        let matching: Vec<usize> = (0..options.len())
            .filter(|&i| {
                let o = &options[i];
                eligible(o)
                    && o.roles.forced
                    && o.language
                        .as_deref()
                        .is_some_and(|l| same_language(l, audio))
            })
            .collect();
        (matching.len() == 1).then(|| matching[0])
    };
    if require {
        return match full() {
            Ok(Some(i)) => AutomaticSubtitle::Selected(i),
            Ok(None) => AutomaticSubtitle::Unavailable,
            Err(()) => AutomaticSubtitle::Ambiguous,
        };
    }
    let chosen = match mode {
        SubtitleMode::Off => None,
        SubtitleMode::Forced => forced(),
        SubtitleMode::Always => full().ok().flatten(),
        SubtitleMode::ForeignAudio => {
            let understood = audio_languages.iter().chain(subtitle_languages);
            let foreign = audio_language.is_some_and(|audio| {
                audio_languages.len() + subtitle_languages.len() > 0
                    && !understood.clone().any(|l| same_language(l, audio))
            });
            if foreign {
                full().ok().flatten().or_else(forced)
            } else {
                forced()
            }
        }
    };
    chosen.map_or(AutomaticSubtitle::None, AutomaticSubtitle::Selected)
}

/// Lower-case language tag; `und`, empty or malformed tags mean unknown.
pub fn normalize_language(tag: Option<&str>) -> Option<String> {
    let tag = tag?.trim().to_ascii_lowercase();
    let valid = !tag.is_empty()
        && tag.len() <= 35
        && tag
            .split(['-', '_'])
            .all(|p| !p.is_empty() && p.len() <= 8 && p.bytes().all(|b| b.is_ascii_alphanumeric()));
    (valid && tag != "und").then(|| tag.replace('_', "-"))
}

fn key(o: &Observed) -> (Kind, Option<String>, Roles, Option<String>) {
    (
        o.kind,
        normalize_language(o.language.as_deref()),
        o.roles.clone(),
        o.title
            .as_deref()
            .map(crate::matching::normalize_title)
            .filter(|t| !t.is_empty()),
    )
}

/// Derive components from semantic evidence. An ambiguous key in any file
/// does not establish correspondence with another file's track ordinal.
pub fn derive(timeline: &str, versions: &[(Id, Vec<Observed>)]) -> Vec<Component> {
    type Key = (Kind, Option<String>, Roles, Option<String>);
    let mut ambiguous = BTreeSet::new();
    for (_, tracks) in versions {
        let mut counts = BTreeMap::new();
        for track in tracks {
            let k = key(track);
            let count = counts
                .entry((&track.file_id, &track.file_revision, k.clone()))
                .or_insert(0);
            *count += 1;
            if *count > 1 {
                ambiguous.insert(k);
            }
        }
    }
    let mut components: BTreeMap<(Key, u32, Option<(Id, Id, String)>), Component> = BTreeMap::new();
    for (version, tracks) in versions {
        // Ordinals count same-key tracks within one file, so each part of a
        // multipart version continues the same components.
        #[allow(clippy::type_complexity)]
        let mut seen: BTreeMap<(Id, Kind, Option<String>, Roles, Option<String>), u32> =
            BTreeMap::new();
        let mut ordered: Vec<&Observed> = tracks.iter().collect();
        ordered.sort_by(|a, b| {
            (&a.file_id, a.sidecar.is_some(), a.track_index, &a.sidecar).cmp(&(
                &b.file_id,
                b.sidecar.is_some(),
                b.track_index,
                &b.sidecar,
            ))
        });
        for track in ordered {
            let k = key(track);
            let ordinal = {
                let n = seen
                    .entry((
                        track.file_id.clone(),
                        k.0,
                        k.1.clone(),
                        k.2.clone(),
                        k.3.clone(),
                    ))
                    .or_insert(0);
                *n += 1;
                *n
            };
            let (kind, language, roles, title) = k;
            let component = components
                .entry((
                    (kind, language.clone(), roles.clone(), title.clone()),
                    ordinal,
                    ambiguous
                        .contains(&(kind, language.clone(), roles.clone(), title.clone()))
                        .then(|| {
                            (
                                version.clone(),
                                track.file_id.clone(),
                                track.file_revision.clone(),
                            )
                        }),
                ))
                .or_insert_with(|| Component {
                    id: if ambiguous.contains(&(
                        kind,
                        language.clone(),
                        roles.clone(),
                        title.clone(),
                    )) {
                        // Bounded wire identity with unambiguous field boundaries.
                        // Hash exactly the grouping key, not incidental observations.
                        let identity = serde_json::to_vec(&(
                            "motion-component-v2",
                            timeline,
                            (kind, &language, &roles, &title),
                            ordinal,
                            (version, &track.file_id, &track.file_revision),
                        ))
                        .expect("component identity is serializable");
                        format!("cmp_{:x}", Sha256::digest(identity))
                    } else {
                        // Preserve existing unambiguous identities during the rewrite.
                        format!(
                            "cmp:{timeline}:{}:{}:{}{}{}:{}:{ordinal}",
                            match kind {
                                Kind::Audio => "a",
                                Kind::Subtitle => "s",
                            },
                            language.as_deref().unwrap_or("und"),
                            u8::from(roles.forced),
                            u8::from(roles.hearing_impaired),
                            u8::from(roles.commentary),
                            title.as_deref().unwrap_or("").replace([':', ' '], "_"),
                        )
                    },
                    kind,
                    language,
                    title: track.title.clone(),
                    roles,
                    ordinal,
                    occurrences: vec![],
                });
            component.occurrences.push(Occurrence {
                version_id: version.clone(),
                file_id: track.file_id.clone(),
                file_revision: track.file_revision.clone(),
                track_index: track.track_index,
                sidecar: track.sidecar.clone(),
                default: track.default,
            });
        }
    }
    components.into_values().collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarSubtitle {
    pub language: Option<String>,
    pub roles: Roles,
    pub format: String,
}

/// Recognize `<stem>[.<lang>][.forced|.sdh|.cc|.hi]*.<srt|vtt|ass|ssa>` as a
/// sidecar subtitle of the media file with that stem. Case-sensitive stem
/// match; unknown tokens make the name unrecognized rather than guessed.
pub fn sidecar_subtitle(media_stem: &str, file_name: &str) -> Option<SidecarSubtitle> {
    let rest = file_name.strip_prefix(media_stem)?.strip_prefix('.')?;
    let (body, extension) = rest.rsplit_once('.').unwrap_or(("", rest));
    let format = extension.to_ascii_lowercase();
    if !matches!(format.as_str(), "srt" | "vtt" | "ass" | "ssa") {
        return None;
    }
    let mut out = SidecarSubtitle {
        language: None,
        roles: Roles::default(),
        format,
    };
    for token in body.split('.').filter(|t| !t.is_empty()) {
        match token.to_ascii_lowercase().as_str() {
            "forced" => out.roles.forced = true,
            "sdh" | "cc" | "hi" => out.roles.hearing_impaired = true,
            other if out.language.is_none() => {
                out.language = Some(normalize_language(Some(other))?)
            }
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn track(file: &str, index: i64, kind: Kind, lang: &str, roles: Roles) -> Observed {
        Observed {
            file_id: file.into(),
            file_revision: format!("r-{file}"),
            kind,
            language: Some(lang.into()),
            title: None,
            roles,
            default: false,
            track_index: Some(index),
            sidecar: None,
        }
    }

    #[test]
    fn embedded_selection_never_substitutes_kind_cut_file_revision_or_sidecar() {
        let mut components = derive(
            "t",
            &[(
                "v".into(),
                vec![track("f", 2, Kind::Audio, "en", Roles::default())],
            )],
        );
        let id = components[0].id.clone();
        assert_eq!(
            embedded_track(&components, &id, Kind::Audio, "v", "f", "r-f"),
            Some(2)
        );
        for (kind, version, file, revision) in [
            (Kind::Subtitle, "v", "f", "r-f"),
            (Kind::Audio, "other", "f", "r-f"),
            (Kind::Audio, "v", "other", "r-f"),
            (Kind::Audio, "v", "f", "stale"),
        ] {
            assert_eq!(
                embedded_track(&components, &id, kind, version, file, revision),
                None
            );
        }
        components[0].occurrences[0].sidecar = Some("captions.srt".into());
        assert_eq!(
            embedded_track(&components, &id, Kind::Audio, "v", "f", "r-f"),
            None
        );
    }

    #[test]
    fn ambiguous_components_do_not_guess_cross_file_correspondence() {
        let commentary = Roles {
            commentary: true,
            ..Default::default()
        };
        let v1 = vec![
            track("f1", 1, Kind::Audio, "en", Roles::default()),
            track("f1", 2, Kind::Audio, "en", commentary.clone()),
            track("f1", 3, Kind::Audio, "EN", Roles::default()),
        ];
        let v2 = vec![
            track("f2", 2, Kind::Audio, "en", commentary.clone()),
            track("f2", 1, Kind::Audio, "en", Roles::default()),
        ];
        let c = derive("t", &[("v1".into(), v1), ("v2".into(), v2)]);
        assert_eq!(
            c.len(),
            4,
            "three distinct ambiguous tracks and shared commentary"
        );
        // A two-part version with one English track per part is one component.
        let parts = derive(
            "t",
            &[(
                "multi".into(),
                vec![
                    track("p1", 1, Kind::Audio, "en", Roles::default()),
                    track("p2", 1, Kind::Audio, "en", Roles::default()),
                ],
            )],
        );
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].occurrences.len(), 2);
        assert_eq!(
            sidecar_subtitle("Movie", "Movie.zh-Hant-TW.srt")
                .unwrap()
                .language
                .as_deref(),
            Some("zh-hant-tw")
        );
        let main = c
            .iter()
            .find(|c| !c.roles.commentary && c.ordinal == 1)
            .unwrap();
        assert_eq!(main.occurrences.len(), 1);
        assert_eq!(main.id.len(), 68);
        assert!(
            main.id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        );
        assert!(
            c.iter()
                .filter(|c| !c.roles.commentary)
                .all(|c| c.occurrences.len() == 1)
        );
        let comm = c.iter().find(|c| c.roles.commentary).unwrap();
        assert_eq!(
            comm.occurrences
                .iter()
                .map(|o| o.track_index)
                .collect::<Vec<_>>(),
            [Some(2), Some(2)]
        );
        let second = c.iter().find(|c| c.ordinal == 2).unwrap();
        assert_eq!(second.occurrences.len(), 1);
        // A formerly unambiguous identity must not select an ambiguous ordinal.
        assert_ne!(
            derive(
                "t",
                &[(
                    "v1".into(),
                    vec![track("f1", 1, Kind::Audio, "en", Roles::default())]
                )]
            )[0]
            .id,
            main.id
        );
    }
    #[test]
    fn sidecar_names_are_parsed_strictly() {
        let s = sidecar_subtitle("Movie (2001)", "Movie (2001).en.forced.srt").unwrap();
        assert_eq!(s.language.as_deref(), Some("en"));
        assert!(s.roles.forced && !s.roles.hearing_impaired);
        assert_eq!(s.format, "srt");
        let s = sidecar_subtitle("Movie", "Movie.pt-BR.sdh.VTT").unwrap();
        assert_eq!(s.language.as_deref(), Some("pt-br"));
        assert!(s.roles.hearing_impaired);
        assert_eq!(
            sidecar_subtitle("Movie", "Movie.srt").unwrap().language,
            None
        );
        assert!(sidecar_subtitle("Movie", "Movie2.en.srt").is_none());
        assert!(sidecar_subtitle("Movie", "Movie.en.director.srt").is_none());
        assert!(sidecar_subtitle("Movie", "Movie.en.nfo").is_none());
        assert_eq!(normalize_language(Some("und")), None);
    }

    #[test]
    fn subtitle_pins_resolve_only_on_the_chosen_revision_bound_file() {
        let occurrence =
            |version: &str, file: &str, index: Option<i64>, sidecar: Option<&str>| Occurrence {
                version_id: version.into(),
                file_id: file.into(),
                file_revision: format!("r-{file}"),
                track_index: index,
                sidecar: sidecar.map(Into::into),
                default: false,
            };
        let component = |id: &str, kind, occurrences| Component {
            id: id.into(),
            kind,
            language: Some("en".into()),
            title: None,
            roles: Roles::default(),
            ordinal: 1,
            occurrences,
        };
        let components = [
            component(
                "embedded",
                Kind::Subtitle,
                vec![
                    occurrence("v1", "f1", Some(3), None),
                    occurrence("v2", "f2", Some(2), None),
                ],
            ),
            component(
                "sidecar",
                Kind::Subtitle,
                vec![occurrence("v1", "f1", None, Some("film.en.srt"))],
            ),
            component(
                "audio",
                Kind::Audio,
                vec![occurrence("v1", "f1", Some(1), None)],
            ),
            component(
                "ambiguous",
                Kind::Subtitle,
                vec![
                    occurrence("v1", "f1", Some(4), None),
                    occurrence("v1", "f1", Some(5), None),
                ],
            ),
        ];
        let resolve = |id, version, file, revision| {
            subtitle_occurrence(&components, id, version, file, revision)
        };
        assert_eq!(
            resolve("embedded", "v1", "f1", "r-f1"),
            Some(SubtitleOccurrence::Embedded(3))
        );
        assert_eq!(
            resolve("embedded", "v2", "f2", "r-f2"),
            Some(SubtitleOccurrence::Embedded(2))
        );
        assert_eq!(
            resolve("sidecar", "v1", "f1", "r-f1"),
            Some(SubtitleOccurrence::Sidecar("film.en.srt".into()))
        );
        // Another file, revision or version never substitutes.
        assert_eq!(resolve("sidecar", "v2", "f2", "r-f2"), None);
        assert_eq!(resolve("embedded", "v1", "f1", "stale"), None);
        assert_eq!(resolve("embedded", "v2", "f1", "r-f1"), None);
        // Audio components and ambiguous occurrences are not subtitles to pin.
        assert_eq!(resolve("audio", "v1", "f1", "r-f1"), None);
        assert_eq!(resolve("ambiguous", "v1", "f1", "r-f1"), None);
        assert_eq!(resolve("missing", "v1", "f1", "r-f1"), None);
    }

    #[test]
    fn languages_match_by_primary_subtag_across_iso_639_forms() {
        assert!(same_language("eng", "en"));
        assert!(same_language("en-US", "EN"));
        assert!(same_language("fre", "fra") && same_language("fr", "fre"));
        assert!(!same_language("en", "fr"));
        assert!(!same_language("und", "und"));
        assert!(!same_language("", "en"));
    }

    #[test]
    fn automatic_subtitles_follow_profile_policy_and_never_substitute() {
        let option = |language: &str, forced, sdh, commentary, deliverable| SubtitleOption {
            language: Some(language.into()),
            roles: Roles {
                forced,
                hearing_impaired: sdh,
                commentary,
            },
            deliverable,
        };
        let options = [
            option("eng", false, false, false, true),  // 0 full English
            option("eng", false, true, false, true),   // 1 English SDH
            option("fre", true, false, false, true),   // 2 forced French
            option("spa", false, false, true, true),   // 3 Spanish commentary
            option("ger", false, false, false, false), // 4 German, undeliverable
            option("eng", true, false, false, true),   // 5 forced English
        ];
        let en = ["en".to_string()];
        let auto = |mode, audio| automatic_subtitle(false, mode, audio, &en, &en, &options);
        use AutomaticSubtitle::{Ambiguous, Selected, Unavailable};
        // English audio: only the forced English track, never full subtitles.
        assert_eq!(auto(SubtitleMode::ForeignAudio, Some("eng")), Selected(5));
        assert_eq!(auto(SubtitleMode::Forced, Some("eng")), Selected(5));
        // French audio is foreign: full English subtitles (plain over SDH).
        assert_eq!(auto(SubtitleMode::ForeignAudio, Some("fre")), Selected(0));
        assert_eq!(auto(SubtitleMode::Forced, Some("fre")), Selected(2));
        assert_eq!(auto(SubtitleMode::Always, Some("eng")), Selected(0));
        assert_eq!(
            auto(SubtitleMode::Off, Some("fre")),
            AutomaticSubtitle::None
        );
        // Unknown audio language: no forced track can be chosen.
        assert_eq!(auto(SubtitleMode::Forced, None), AutomaticSubtitle::None);
        // Require: preferred languages in order, never another language.
        let require = |languages: &[String], options: &[SubtitleOption]| {
            automatic_subtitle(
                true,
                SubtitleMode::Off,
                Some("eng"),
                &[],
                languages,
                options,
            )
        };
        assert_eq!(require(&en, &options), Selected(0));
        assert_eq!(require(&["de".into()], &options), Unavailable);
        assert_eq!(require(&["es".into()], &options), Unavailable);
        assert_eq!(require(&[], &options), Unavailable);
        assert_eq!(require(&["de".into(), "en".into()], &options), Selected(0));
        // Two plain English tracks: the client must choose.
        let tied = [options[0].clone(), options[0].clone()];
        assert_eq!(require(&en, &tied), Ambiguous);
        assert_eq!(
            automatic_subtitle(false, SubtitleMode::Always, None, &[], &en, &tied),
            AutomaticSubtitle::None
        );
        // Only SDH available: it is chosen.
        assert_eq!(require(&en, &[options[1].clone()]), Selected(0));
        // Foreign-audio with no listed languages cannot call any audio foreign.
        assert_eq!(
            automatic_subtitle(
                false,
                SubtitleMode::ForeignAudio,
                Some("fre"),
                &[],
                &[],
                &options
            ),
            Selected(2)
        );
    }

    #[test]
    fn a_component_maps_to_its_own_track_in_every_part() {
        // Two English stereo tracks distinguished only by title, in opposite
        // orders in the two parts of one version.
        let track = |file: &str, index, title: &str| Observed {
            file_id: file.into(),
            file_revision: format!("r-{file}"),
            kind: Kind::Audio,
            language: Some("eng".into()),
            title: Some(title.into()),
            roles: Roles::default(),
            default: false,
            track_index: Some(index),
            sidecar: None,
        };
        let components = derive(
            "t",
            &[(
                "v".into(),
                vec![
                    track("p1", 1, "Stereo"),
                    track("p1", 2, "Director"),
                    track("p2", 1, "Director"),
                    track("p2", 2, "Stereo"),
                ],
            )],
        );
        let files = [("p1", "r-p1"), ("p2", "r-p2")];
        let stereo = component_of(&components, Kind::Audio, "v", "p1", "r-p1", 1).unwrap();
        assert_eq!(per_file_tracks(stereo, "v", &files), Some(vec![1, 2]));
        let director = component_of(&components, Kind::Audio, "v", "p1", "r-p1", 2).unwrap();
        assert_eq!(per_file_tracks(director, "v", &files), Some(vec![2, 1]));
        // A part without an occurrence (or at another revision) has no mapping.
        assert_eq!(
            per_file_tracks(stereo, "v", &[("p1", "r-p1"), ("p3", "r-p3")]),
            None
        );
        assert_eq!(
            per_file_tracks(stereo, "v", &[("p1", "r-p1"), ("p2", "stale")]),
            None
        );
        assert_eq!(
            component_of(&components, Kind::Audio, "v", "p1", "r-p1", 9),
            None
        );
    }
}
