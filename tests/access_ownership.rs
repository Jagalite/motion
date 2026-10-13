//! Real SQLite checks for unassigned legacy devices and explicit ownership.
use playscale::{db, v2::auth};
use playscale_core::access::{Device, Grant, Permission, Policy};

fn device(id: &str, owner: Option<&str>) -> auth::DeviceRow {
    auth::DeviceRow {
        device: Device::approved(
            id.into(),
            Grant {
                permissions: [Permission::CatalogRead].into(),
                profile_ids: ["default".into()].into(),
            },
        ),
        name: "Same display name".into(),
        client_name: "ownership-test".into(),
        user_id: owner.map(str::to_owned),
    }
}

#[tokio::test]
async fn unassigned_insertion_preserves_grants_without_creating_users() {
    let directory = tempfile::tempdir().unwrap();
    let pool = db::connect(&directory.path().join("db.sqlite"))
        .await
        .unwrap();
    let mut tx = db::begin_write(&pool).await.unwrap();
    for id in ["first", "second"] {
        let row = device(id, None);
        auth::insert_device(&mut tx, &row).await.unwrap();
        let observed = auth::load_device(&mut tx, id).await.unwrap().unwrap();
        assert_eq!(observed.device, row.device);
        assert_eq!(observed.user_id, None);
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM access_users")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(count, 0);
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn owned_admission_observes_current_owner_and_never_falls_back_to_legacy() {
    let directory = tempfile::tempdir().unwrap();
    let pool = db::connect(&directory.path().join("db.sqlite"))
        .await
        .unwrap();
    let mut tx = db::begin_write(&pool).await.unwrap();
    let row = device("owned", Some("person"));
    sqlx::query("INSERT INTO access_users(id,name,revision,profile_ids,permissions,policy,created_at) VALUES ('person','Person',1,?,?,?,0)")
        .bind(serde_json::to_string(&row.device.grant.profile_ids).unwrap())
        .bind(serde_json::to_string(&row.device.grant.permissions).unwrap())
        .bind(serde_json::to_string(&Policy::default()).unwrap())
        .execute(&mut *tx).await.unwrap();
    auth::insert_device(&mut tx, &row).await.unwrap();
    assert!(
        !auth::load_device(&mut tx, "owned")
            .await
            .unwrap()
            .unwrap()
            .device
            .revoked
    );
    sqlx::query("UPDATE access_users SET disabled=1,revision=2 WHERE id='person'")
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(
        auth::load_device(&mut tx, "owned")
            .await
            .unwrap()
            .unwrap()
            .device
            .revoked
    );
    let rejected = auth::insert_device(&mut tx, &device("another", Some("person")))
        .await
        .unwrap_err();
    assert_eq!(rejected.code, "user_disabled");
    let absent = auth::insert_device(&mut tx, &device("missing", Some("unknown")))
        .await
        .unwrap_err();
    assert_eq!(absent.code, "user_disabled");
    sqlx::query("UPDATE access_users SET disabled=0,permissions='[]',revision=3 WHERE id='person'")
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(
        auth::load_device(&mut tx, "owned")
            .await
            .unwrap()
            .unwrap()
            .device
            .revoked
    );
    // Observed denial is not an adapter write or credential resurrection.
    let persisted: bool = sqlx::query_scalar("SELECT revoked FROM devices WHERE id='owned'")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert!(!persisted);
    tx.commit().await.unwrap();
}
