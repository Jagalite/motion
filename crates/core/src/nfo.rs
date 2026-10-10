//! Bounded NFO (Kodi-style XML sidecar) parsing for a deliberately supported
//! dialect. Parsing is local evidence only: it yields a metadata contribution
//! and external identities; it never writes catalog identity by itself.
//!
//! Safety: no DTDs, entity declarations, external references or network
//! access. Only the five predefined entities and numeric character references
//! are decoded. Input size, nesting depth, element count and text length are
//! capped. Anything outside the supported subset is ignored, not guessed.
//!
//! Supported roots: `movie`, `tvshow`, `episodedetails`. Supported fields:
//! `title`, `originaltitle`, `year`, `premiered`/`aired` (YYYY-MM-DD), `plot`,
//! `genre` (repeatable), `tag` (repeatable), `actor/name` + `actor/role`,
//! `uniqueid type=".." [default="true"]`, legacy `id` (IMDb `tt…` only),
//! `tmdbid`/`imdbid`,
//! `season`/`episode` numbers. Unsupported: ratings, artwork URLs (never
//! fetched), stream details, sets, studios, credits beyond actors.
use crate::metadata::{Contribution, valid_field};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const MAX_BYTES: usize = 256 * 1024;
pub const MAX_DEPTH: usize = 16;
pub const MAX_ELEMENTS: usize = 4096;
pub const MAX_TEXT: usize = 8000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NfoError {
    TooLarge,
    NotUtf8,
    Declaration,
    Malformed(usize),
    TooDeep,
    TooManyElements,
    UnsupportedRoot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Element {
    name: String,
    attributes: BTreeMap<String, String>,
    text: String,
    children: Vec<Element>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Nfo {
    pub kind: String,
    pub contribution: Contribution,
    /// `(provider, value)` pairs, the default one first.
    pub external_ids: Vec<(String, String)>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
}

fn decode_entities(raw: &str, at: usize) -> Result<String, NfoError> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let end = tail
            .find(';')
            .filter(|e| *e <= 10)
            .ok_or(NfoError::Malformed(at))?;
        let name = &tail[..end];
        let ch = match name {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            n if n.starts_with("#x") => u32::from_str_radix(&n[2..], 16)
                .ok()
                .and_then(char::from_u32)
                .ok_or(NfoError::Malformed(at))?,
            n if n.starts_with('#') => n[1..]
                .parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .ok_or(NfoError::Malformed(at))?,
            _ => return Err(NfoError::Malformed(at)),
        };
        out.push(ch);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
}

/// Parse the supported XML subset into an element tree.
fn parse_tree(input: &[u8]) -> Result<Element, NfoError> {
    if input.len() > MAX_BYTES {
        return Err(NfoError::TooLarge);
    }
    let text = std::str::from_utf8(input).map_err(|_| NfoError::NotUtf8)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut stack: Vec<Element> = vec![Element {
        name: String::new(),
        attributes: BTreeMap::new(),
        text: String::new(),
        children: vec![],
    }];
    let mut count = 0usize;
    let mut i = 0usize;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        if bytes[i] != b'<' {
            let end = text[i..].find('<').map_or(bytes.len(), |e| i + e);
            let chunk = decode_entities(&text[i..end], i)?;
            let top = stack.last_mut().unwrap();
            if top.text.len() + chunk.len() > MAX_TEXT {
                return Err(NfoError::TooLarge);
            }
            top.text.push_str(&chunk);
            i = end;
            continue;
        }
        let rest = &text[i..];
        if rest.starts_with("<?") {
            i += rest.find("?>").ok_or(NfoError::Malformed(i))? + 2;
        } else if rest.starts_with("<!--") {
            i += rest.find("-->").ok_or(NfoError::Malformed(i))? + 3;
        } else if rest.starts_with("<![CDATA[") {
            let end = rest.find("]]>").ok_or(NfoError::Malformed(i))?;
            let top = stack.last_mut().unwrap();
            if top.text.len() + end > MAX_TEXT {
                return Err(NfoError::TooLarge);
            }
            top.text.push_str(&rest[9..end]);
            i += end + 3;
        } else if rest.starts_with("<!") {
            // DOCTYPE, ENTITY and every other declaration are refused outright.
            return Err(NfoError::Declaration);
        } else if let Some(close) = rest.strip_prefix("</") {
            let end = close.find('>').ok_or(NfoError::Malformed(i))?;
            let name = close[..end].trim();
            let element = stack.pop().ok_or(NfoError::Malformed(i))?;
            if element.name != name || stack.is_empty() {
                return Err(NfoError::Malformed(i));
            }
            stack.last_mut().unwrap().children.push(element);
            i += 2 + end + 1;
        } else {
            let end = rest.find('>').ok_or(NfoError::Malformed(i))?;
            let inner = &rest[1..end];
            let self_closing = inner.ends_with('/');
            let inner = inner.trim_end_matches('/');
            let (name, attrs) = inner
                .split_once(|c: char| c.is_whitespace())
                .unwrap_or((inner, ""));
            if !valid_name(name) {
                return Err(NfoError::Malformed(i));
            }
            let mut attributes = BTreeMap::new();
            let mut a = attrs.trim();
            while !a.is_empty() {
                let eq = a.find('=').ok_or(NfoError::Malformed(i))?;
                let key = a[..eq].trim();
                let after = a[eq + 1..].trim_start();
                let quote = after.chars().next().ok_or(NfoError::Malformed(i))?;
                if quote != '"' && quote != '\'' {
                    return Err(NfoError::Malformed(i));
                }
                let close = after[1..].find(quote).ok_or(NfoError::Malformed(i))?;
                if !valid_name(key) {
                    return Err(NfoError::Malformed(i));
                }
                attributes.insert(key.to_string(), decode_entities(&after[1..1 + close], i)?);
                a = after[1 + close + 1..].trim_start();
            }
            count += 1;
            if count > MAX_ELEMENTS {
                return Err(NfoError::TooManyElements);
            }
            let element = Element {
                name: name.to_string(),
                attributes,
                text: String::new(),
                children: vec![],
            };
            if self_closing {
                stack.last_mut().unwrap().children.push(element);
            } else {
                if stack.len() > MAX_DEPTH {
                    return Err(NfoError::TooDeep);
                }
                stack.push(element);
            }
            i += end + 1;
        }
    }
    if stack.len() != 1 {
        return Err(NfoError::Malformed(bytes.len()));
    }
    let mut document = stack.pop().unwrap();
    if document.children.len() != 1 || !document.text.trim().is_empty() {
        return Err(NfoError::Malformed(0));
    }
    Ok(document.children.remove(0))
}

fn text_of<'a>(root: &'a Element, name: &str) -> Option<&'a str> {
    root.children
        .iter()
        .find(|c| c.name == name)
        .map(|c| c.text.trim())
        .filter(|t| !t.is_empty())
}
fn all_text<'a>(root: &'a Element, name: &str) -> impl Iterator<Item = &'a str> {
    root.children
        .iter()
        .filter(move |c| c.name == name)
        .map(|c| c.text.trim())
        .filter(|t| !t.is_empty())
}
fn provider(name: &str) -> Option<String> {
    let p = name.trim().to_ascii_lowercase();
    (!p.is_empty() && p.len() <= 32 && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        .then_some(p)
}

/// Parse NFO bytes into a validated contribution and identities.
pub fn parse(input: &[u8]) -> Result<Nfo, NfoError> {
    let root = parse_tree(input)?;
    if !matches!(root.name.as_str(), "movie" | "tvshow" | "episodedetails") {
        return Err(NfoError::UnsupportedRoot);
    }
    let mut values = BTreeMap::new();
    let mut put = |key: &str, value: Value| {
        if valid_field(key, &value) {
            values.insert(key.to_string(), value);
        }
    };
    if let Some(title) = text_of(&root, "title") {
        put("title", json!(title));
    }
    if let Some(original) = text_of(&root, "originaltitle") {
        put("original_title", json!(original));
    }
    if let Some(year) = text_of(&root, "year").and_then(|y| y.parse::<i64>().ok()) {
        put("release_year", json!(year));
    }
    if let Some(date) = text_of(&root, "premiered").or_else(|| text_of(&root, "aired")) {
        put("release_date", json!(date));
    }
    if let Some(plot) = text_of(&root, "plot") {
        put("description", json!(plot));
    }
    let genres: Vec<&str> = all_text(&root, "genre").take(50).collect();
    if !genres.is_empty() {
        put("genres", json!(genres));
    }
    let cast: Vec<Value> = root
        .children
        .iter()
        .filter(|c| c.name == "actor")
        .filter_map(|actor| {
            let name = text_of(actor, "name")?;
            Some(match text_of(actor, "role") {
                Some(role) => json!({"name": name, "role": role}),
                None => json!({"name": name}),
            })
        })
        .take(100)
        .collect();
    if !cast.is_empty() {
        put("cast", Value::Array(cast));
    }
    let tags = all_text(&root, "tag")
        .map(|t| {
            t.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        })
        .filter(|t| t.len() <= 100)
        .take(100)
        .collect();
    let mut external_ids: Vec<(String, String, bool)> = root
        .children
        .iter()
        .filter(|c| c.name == "uniqueid")
        .filter_map(|c| {
            let p = provider(c.attributes.get("type")?)?;
            let v = c.text.trim();
            (!v.is_empty() && v.len() <= 256).then(|| {
                (
                    p,
                    v.to_string(),
                    c.attributes.get("default").is_some_and(|d| d == "true"),
                )
            })
        })
        .collect();
    for (legacy, p) in [("tmdbid", "tmdb"), ("imdbid", "imdb")] {
        if let Some(v) = text_of(&root, legacy).filter(|v| v.len() <= 256)
            && !external_ids.iter().any(|(q, _, _)| q == p)
        {
            external_ids.push((p.into(), v.into(), false));
        }
    }
    // Kodi's legacy `<id>` holds an IMDb identifier for movies; other values
    // are ambiguous across providers and are not guessed.
    if let Some(v) = text_of(&root, "id").filter(|v| v.starts_with("tt") && v.len() <= 32)
        && !external_ids.iter().any(|(q, _, _)| q == "imdb")
    {
        external_ids.push(("imdb".into(), v.into(), false));
    }
    external_ids.sort_by_key(|(_, _, default)| !*default);
    let number = |name: &str| text_of(&root, name).and_then(|n| n.parse::<u32>().ok());
    Ok(Nfo {
        kind: root.name.clone(),
        contribution: Contribution {
            values,
            tags,
            excluded_tags: Default::default(),
        },
        external_ids: external_ids.into_iter().map(|(p, v, _)| (p, v)).collect(),
        season: number("season"),
        episode: number("episode"),
    })
}
