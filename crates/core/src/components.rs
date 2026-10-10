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
use std::collections::BTreeMap;

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

/// Derive a timeline's components from its versions' observed tracks. Within
/// one version, the n-th track with a key is component ordinal n; the same
/// ordinal in another version is another occurrence of that component.
pub fn derive(timeline: &str, versions: &[(Id, Vec<Observed>)]) -> Vec<Component> {
    #[allow(clippy::type_complexity)]
    let mut components: BTreeMap<
        (Kind, Option<String>, Roles, Option<String>, u32),
        Component,
    > = BTreeMap::new();
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
                    kind,
                    language.clone(),
                    roles.clone(),
                    title.clone(),
                    ordinal,
                ))
                .or_insert_with(|| Component {
                    id: format!(
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
                    ),
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
    fn components_group_across_versions_by_key_and_ordinal() {
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
        assert_eq!(c.len(), 3, "main, second main (ordinal 2), commentary");
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
        assert_eq!(main.occurrences.len(), 2);
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
        // Deterministic identities.
        assert_eq!(
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
}
