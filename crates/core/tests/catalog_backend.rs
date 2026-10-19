use playscale_core::catalog_backend::{AdmissionError, Identity};

#[test]
fn admission_checks_every_identity_dimension_without_rebinding() {
    let identities: Vec<_> = ["", "catalog-a", "catalog-b"]
        .into_iter()
        .flat_map(|catalog| {
            ["", "original", "restored"]
                .into_iter()
                .flat_map(move |incarnation| {
                    [0, 37, 38].into_iter().map(move |schema| Identity {
                        catalog_id: catalog.into(),
                        incarnation: incarnation.into(),
                        schema_version: schema,
                    })
                })
        })
        .collect();
    let mut checked = 0;
    for bound in &identities {
        for observed in &identities {
            let unchanged = bound.clone();
            let actual = bound.admit(observed);
            // Independent finite oracle: exact equality AND both identities valid.
            let valid = [&bound, &observed].iter().all(|i| {
                !i.catalog_id.is_empty() && !i.incarnation.is_empty() && i.schema_version > 0
            });
            assert_eq!(actual.is_ok(), valid && bound == observed);
            assert_eq!(*bound, unchanged);
            checked += 1;
        }
    }
    assert_eq!(checked, 729);
}

#[test]
fn restore_is_distinct_from_replacement_and_schema_drift() {
    let bound = Identity {
        catalog_id: "a".into(),
        incarnation: "original".into(),
        schema_version: 38,
    };
    let mut observed = bound.clone();
    observed.incarnation = "restored".into();
    assert_eq!(bound.admit(&observed), Err(AdmissionError::RestoreDetected));
    observed = bound.clone();
    observed.catalog_id = "b".into();
    assert_eq!(bound.admit(&observed), Err(AdmissionError::CatalogChanged));
    observed = bound.clone();
    observed.schema_version = 39;
    assert_eq!(bound.admit(&observed), Err(AdmissionError::SchemaChanged));
}

#[test]
fn export_owner_grants_require_exact_supported_source_provenance() {
    use playscale_core::catalog_backend::ExportProvenance;
    let digests = [
        String::new(),
        "a".repeat(64),
        "b".repeat(64),
        "x".repeat(64),
    ];
    let namespaces = ["", "motion", "other"];
    let mut checked = 0;
    for saved_digest in &digests {
        for saved_namespace in namespaces {
            for version in [0, 1, 2] {
                let proof = ExportProvenance {
                    input_digest: saved_digest.clone(),
                    source_namespace: saved_namespace.into(),
                    format_version: version,
                };
                for digest in &digests {
                    for namespace in namespaces {
                        let expected = version == 1
                            && matches!(saved_digest.as_bytes().first(), Some(b'a' | b'b'))
                            && !saved_namespace.is_empty()
                            && saved_digest == digest
                            && saved_namespace == namespace;
                        assert_eq!(proof.admits(digest, namespace), expected);
                        checked += 1;
                    }
                }
            }
        }
    }
    assert_eq!(checked, 432);
}

#[test]
fn export_pins_preserve_live_manual_identity_protection() {
    use playscale_core::{catalog_backend::pinned_source_identity, matching::MatchState};
    let current = ("tmdb".into(), "42".into());
    let mut checked = 0;
    for state in [
        MatchState::Unmatched,
        MatchState::Ambiguous,
        MatchState::Matched,
        MatchState::Manual,
    ] {
        for retired in [false, true] {
            for identity in [None, Some(&current)] {
                assert_eq!(
                    pinned_source_identity(state, retired, identity),
                    state == MatchState::Manual && !retired && identity.is_some()
                );
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 16);
}
