//! Actual retained-native drain, before preview job/effect lookup.
use crate as playscale_app;
#[path = "../../../tests/support/native_application_fixture.rs"]
mod native_application_fixture;
use native_application_fixture::Fixture;

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_preview_finish_normalizes_only_actual_pre_effect_drain() {
    let mut f = Fixture::new().await;
    f.cutover().await;
    let lease = std::sync::Arc::new(
        f.app
            .processing
            .execution
            .reserve(
                "preview-drain-test".to_owned(),
                playscale_core::work::Class::Maintenance,
                1,
            )
            .await
            .unwrap(),
    );
    let job = super::Started {
        id: "no-job-must-be-read".to_owned(),
        attempt: 1,
        file: "no-file-must-be-read".to_owned(),
        revision: "no-revision".to_owned(),
    };
    f.app.native_catalog.as_ref().unwrap().begin_drain();
    let start_error = match super::start(&f.app).await {
        Err(error) => error,
        Ok(_) => panic!("drained native owner admitted preview intake"),
    };
    assert!(
        start_error
            .downcast_ref::<crate::native_catalog::OrdinaryAdmissionDraining>()
            .is_some()
    );
    let stop = tokio_util::sync::CancellationToken::new();
    stop.cancel();
    super::finish(
        &f.app,
        &job,
        Err(anyhow::anyhow!("conversion cancelled")),
        lease.clone(),
        std::sync::Arc::new(()),
        &stop,
    )
    .await
    .unwrap();
    let uncancelled = tokio_util::sync::CancellationToken::new();
    let error = super::finish(
        &f.app,
        &job,
        Err(anyhow::anyhow!("conversion cancelled")),
        lease.clone(),
        std::sync::Arc::new(()),
        &uncancelled,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .downcast_ref::<crate::native_catalog::OrdinaryAdmissionDraining>()
            .is_some()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM preview_jobs")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
    drop(lease);
    f.close().await;
}
