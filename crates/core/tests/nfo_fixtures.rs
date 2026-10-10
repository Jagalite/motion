//! Hand-authored NFO fixtures: supported dialect and hostile inputs.
use playscale_core::nfo::{MAX_BYTES, MAX_DEPTH, NfoError, parse};
use serde_json::json;

#[test]
fn movie_fields_identities_and_unsupported_parts() {
    let nfo = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<!-- written by a media manager -->
<movie>
  <title>Am&#233;lie &amp; Co</title>
  <originaltitle>Le Fabuleux Destin d&apos;Am&#xE9;lie Poulain</originaltitle>
  <year>2001</year>
  <premiered>2001-04-25</premiered>
  <plot><![CDATA[A <shy> waitress.]]></plot>
  <genre>Comedy</genre><genre>Romance</genre>
  <tag>Favorite  Films</tag>
  <uniqueid type="imdb">tt0211915</uniqueid>
  <uniqueid type="TMDB" default="true">194</uniqueid>
  <actor><name>Audrey Tautou</name><role>Amélie</role><thumb>http://example.invalid/a.jpg</thumb></actor>
  <actor><role>No name is skipped</role></actor>
  <ratings><rating name="imdb"><value>8.3</value></rating></ratings>
  <thumb aspect="poster">http://example.invalid/poster.jpg</thumb>
  <fileinfo><streamdetails/></fileinfo>
</movie>"#;
    let parsed = parse(nfo.as_bytes()).unwrap();
    let v = &parsed.contribution.values;
    assert_eq!(parsed.kind, "movie");
    assert_eq!(v["title"], json!("Amélie & Co"));
    assert_eq!(
        v["original_title"],
        json!("Le Fabuleux Destin d'Amélie Poulain")
    );
    assert_eq!(v["release_year"], json!(2001));
    assert_eq!(v["release_date"], json!("2001-04-25"));
    assert_eq!(v["description"], json!("A <shy> waitress."));
    assert_eq!(v["genres"], json!(["Comedy", "Romance"]));
    assert_eq!(
        v["cast"],
        json!([{"name": "Audrey Tautou", "role": "Amélie"}])
    );
    assert_eq!(
        parsed.contribution.tags.iter().collect::<Vec<_>>(),
        ["favorite films"]
    );
    // The default identity comes first; providers are normalized.
    assert_eq!(
        parsed.external_ids,
        [
            ("tmdb".to_string(), "194".to_string()),
            ("imdb".into(), "tt0211915".into())
        ]
    );
    // Artwork URLs and ratings are not part of the supported dialect.
    assert_eq!(v.len(), 7);
}

#[test]
fn episodes_carry_numbers_and_invalid_values_are_dropped() {
    let parsed = parse(
        b"<episodedetails><title>Pilot</title><season>1</season><episode>2</episode>\
          <aired>2024-02-30</aired><year>twenty</year><tmdbid>42</tmdbid></episodedetails>",
    )
    .unwrap();
    assert_eq!((parsed.season, parsed.episode), (Some(1), Some(2)));
    assert!(
        !parsed.contribution.values.contains_key("release_date"),
        "invalid date"
    );
    assert!(!parsed.contribution.values.contains_key("release_year"));
    assert_eq!(
        parsed.external_ids,
        [("tmdb".to_string(), "42".to_string())]
    );
}

#[test]
fn hostile_inputs_are_refused() {
    let laughs = br#"<?xml version="1.0"?><!DOCTYPE lolz [<!ENTITY lol "lol"><!ENTITY lol2 "&lol;&lol;">]><movie><title>&lol2;</title></movie>"#;
    assert_eq!(parse(laughs), Err(NfoError::Declaration));
    let external = br#"<!DOCTYPE movie SYSTEM "http://example.invalid/x.dtd"><movie/>"#;
    assert_eq!(parse(external), Err(NfoError::Declaration));
    assert!(matches!(
        parse(b"<movie><title>&secret;</title></movie>"),
        Err(NfoError::Malformed(_))
    ));
    let deep = format!(
        "{}{}",
        "<movie>".to_string() + &"<a>".repeat(MAX_DEPTH + 1),
        "</a>".repeat(MAX_DEPTH + 1) + "</movie>"
    );
    assert_eq!(parse(deep.as_bytes()), Err(NfoError::TooDeep));
    assert_eq!(parse(&vec![b' '; MAX_BYTES + 1]), Err(NfoError::TooLarge));
    assert!(matches!(
        parse(b"<movie><title>x</movie></title>"),
        Err(NfoError::Malformed(_))
    ));
    assert!(matches!(
        parse(b"<movie><title>x</title>"),
        Err(NfoError::Malformed(_))
    ));
    assert_eq!(
        parse(b"<musicvideo><title>x</title></musicvideo>"),
        Err(NfoError::UnsupportedRoot)
    );
    assert_eq!(parse(&[0xff, 0xfe, b'<']), Err(NfoError::NotUtf8));
    assert!(matches!(
        parse(b"<movie></movie><movie></movie>"),
        Err(NfoError::Malformed(_))
    ));
    assert!(matches!(
        parse(b"<movie a=b></movie>"),
        Err(NfoError::Malformed(_))
    ));
}

#[test]
fn legacy_id_is_imdb_only_and_never_overrides_uniqueid() {
    let legacy = parse(b"<movie><title>X</title><id>tt0133093</id></movie>").unwrap();
    assert_eq!(
        legacy.external_ids,
        [("imdb".to_string(), "tt0133093".to_string())]
    );
    let numeric = parse(b"<tvshow><title>X</title><id>12345</id></tvshow>").unwrap();
    assert!(
        numeric.external_ids.is_empty(),
        "ambiguous provider is not guessed"
    );
    let explicit =
        parse(br#"<movie><uniqueid type="imdb">tt1</uniqueid><id>tt2</id></movie>"#).unwrap();
    assert_eq!(
        explicit.external_ids,
        [("imdb".to_string(), "tt1".to_string())]
    );
}
