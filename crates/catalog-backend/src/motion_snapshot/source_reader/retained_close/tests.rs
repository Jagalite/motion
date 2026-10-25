use super::super::{SetupFault, main_path, tests::fixture};
use super::*;
use std::{
    future::{Future, poll_fn},
    pin::pin,
    sync::atomic::Ordering,
    task::Poll,
};

fn box_address(slot: &RetainedAuthorizationCloseSlot) -> *const () {
    slot.owner.as_ref().unwrap().close_address()
}
async fn assert_writer_reserved(pool: &sqlx::SqlitePool) {
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(30),
            pool.begin_with("BEGIN IMMEDIATE")
        )
        .await
        .is_err()
    );
}

#[test]
fn empty_close_slot_reserves_same_context_frame_without_allocating() {
    let slot = RetainedAuthorizationCloseSlot::default();
    assert!(!slot.retains_reader_owner());
    assert!(!slot.retains_close());
    assert!(!slot.is_joined());
    assert_eq!(
        RetainedAuthorizationCloseSlot::storage_reservation(),
        std::mem::size_of_val(&slot) as u64 + 128 * 1024 + SOURCE_IDENTITY_RESERVE
    );
    eprintln!(
        "source close inline: {}; source source-custody reserve: {}; source close reserve: {}",
        std::mem::size_of_val(&slot),
        FreshWriter::authorization_storage_reservation(),
        RetainedAuthorizationCloseSlot::storage_reservation()
    );
}

#[tokio::test]
async fn joined_actual_close_keeps_token_closed_and_original_writer_reserved() {
    let (_root, pool, mut writer) = fixture().await;
    let mut token = writer.authorization_reader().await.unwrap();
    let mut slot = RetainedAuthorizationCloseSlot::default();
    token.begin_retained_close(&mut slot).unwrap();
    let original_box = box_address(&slot);
    assert!(slot.retains_close());
    assert!(token.verify().await.is_err());
    assert!(token.authorize_connection().await.is_err());
    assert!(token.retained_allocations().await.is_err());
    assert!(!slot.is_joined());
    slot.join().await.unwrap();
    assert!(slot.is_joined());
    assert_eq!(box_address(&slot), original_box);
    assert!(token.verify().await.is_err());
    slot.join().await.unwrap();
    assert_eq!(box_address(&slot), original_box);
    assert_writer_reserved(&pool).await;
    token.retire_authorization_close(&mut slot).unwrap();
    assert!(!slot.retains_reader_owner());
    assert_writer_reserved(&pool).await;
    let token = writer.authorization_reader().await.unwrap();
    token.retire_inline().await.unwrap();
    writer.close().await.unwrap();
    drop(writer);
    pool.close().await;
}

#[tokio::test]
async fn unpolled_join_and_token_drop_leave_actual_close_box_with_original_writer() {
    let (_root, pool, mut writer) = fixture().await;
    let mut token = writer.authorization_reader().await.unwrap();
    let original_path = token
        .writer
        .authorization
        .reader
        .path()
        .unwrap()
        .to_path_buf();
    let original_fd = token.writer.authorization.reader.file_fd();
    let mut slot = RetainedAuthorizationCloseSlot::default();
    token.begin_retained_close(&mut slot).unwrap();
    let original_box = box_address(&slot);
    drop(slot.join());
    assert_eq!(slot.owner.as_ref().unwrap().file_fd(), original_fd);
    // SAFETY: non-owning observation of still-owned exact FD.
    assert!(unsafe { libc::fcntl(original_fd, libc::F_GETFD) } >= 0);
    drop(token);
    assert!(slot.retains_close());
    assert_eq!(box_address(&slot), original_box);
    assert!(!slot.is_joined());
    assert_eq!(
        main_path(&mut writer.transaction, true).await.unwrap(),
        original_path
    );
    assert_writer_reserved(&pool).await;
    assert!(writer.authorization_reader().await.is_err());
    assert!(writer.close().await.is_err());
    slot.join().await.unwrap();
    assert!(slot.is_joined());
    assert_eq!(box_address(&slot), original_box);
    writer.settle_authorization_close(&mut slot).unwrap();
    assert!(writer.authorization_reader().await.is_err());
    writer.close().await.unwrap();
    drop(slot);
    drop(writer);
    pool.close().await;
}

#[tokio::test]
async fn duplicate_begin_never_takes_another_reader_or_replaces_original_box() {
    let (_a, pa, mut wa) = fixture().await;
    let (_b, pb, mut wb) = fixture().await;
    let mut a = wa.authorization_reader().await.unwrap();
    let mut b = wb.authorization_reader().await.unwrap();
    let mut slot = RetainedAuthorizationCloseSlot::default();
    let mut empty = RetainedAuthorizationCloseSlot::default();
    a.begin_retained_close(&mut slot).unwrap();
    let original_box = box_address(&slot);
    assert!(a.begin_retained_close(&mut slot).is_err());
    assert!(a.begin_retained_close(&mut empty).is_err());
    assert!(!empty.retains_reader_owner());
    assert!(b.begin_retained_close(&mut slot).is_err());
    assert_eq!(box_address(&slot), original_box);
    b.verify().await.unwrap();
    slot.join().await.unwrap();
    b.begin_retained_close(&mut empty).unwrap();
    empty.join().await.unwrap();
    drop(a);
    drop(b);
    assert!(wa.retire_authorization_close(&mut empty).is_err());
    assert!(wb.retire_authorization_close(&mut slot).is_err());
    assert_eq!(box_address(&slot), original_box);
    wa.retire_authorization_close(&mut slot).unwrap();
    wb.retire_authorization_close(&mut empty).unwrap();
    assert!(wa.retire_authorization_close(&mut slot).is_err());
    wa.close().await.unwrap();
    wb.close().await.unwrap();
    drop(wa);
    drop(wb);
    pa.close().await;
    pb.close().await;
}

#[tokio::test]
async fn unpolled_acquisition_claim_is_terminal_without_synthetic_worker_ack() {
    let (_root, pool, mut writer) = fixture().await;
    drop(writer.authorization_reader());
    assert!(writer.authorization_claimed());
    assert!(!writer.retains_authorization_reader());
    assert!(!writer.retains_authorization_acquisition());
    assert!(writer.authorization_reader().await.is_err());
    let mut slot = RetainedAuthorizationCloseSlot::default();
    writer
        .begin_retained_authorization_close(&mut slot)
        .unwrap();
    slot.join().await.unwrap();
    assert!(slot.is_settled());
    assert!(!slot.is_joined());
    assert!(!slot.retains_close());
    assert!(writer.retire_authorization_close(&mut slot).is_err());
    writer.settle_authorization_close(&mut slot).unwrap();
    assert!(writer.authorization_reader().await.is_err());
    writer.close().await.unwrap();
    drop(slot);
    drop(writer);
    pool.close().await;
}

#[tokio::test]
async fn unpolled_prepare_adjacent_future_drop_retains_actual_reader_until_exact_close_ack() {
    async fn prepare(mut reader: RetainedAuthorizationReader<'_>) -> anyhow::Result<()> {
        reader.verify().await?;
        anyhow::bail!("prepare cannot yield authority in this test")
    }
    let (_root, pool, mut writer) = fixture().await;
    let reader = writer.authorization_reader().await.unwrap();
    let fd = reader.writer.authorization.reader.file_fd();
    let opening = reader.writer.authorization.reader.connect_address();
    drop(prepare(reader));
    assert!(writer.retains_authorization_reader());
    assert!(writer.retains_authorization_acquisition());
    assert_eq!(writer.authorization.reader.file_fd(), fd);
    assert_eq!(writer.authorization.reader.connect_address(), opening);
    assert!(writer.authorization_reader().await.is_err());
    let mut slot = RetainedAuthorizationCloseSlot::default();
    writer
        .begin_retained_authorization_close(&mut slot)
        .unwrap();
    assert_eq!(slot.owner.as_ref().unwrap().file_fd(), fd);
    slot.join().await.unwrap();
    assert!(slot.is_joined());
    writer.retire_authorization_close(&mut slot).unwrap();
    writer.close().await.unwrap();
    drop(writer);
    pool.close().await;
}

#[tokio::test]
async fn successful_token_abandonment_retains_resources_and_inline_close_does_not_reopen() {
    let (_root, pool, mut writer) = fixture().await;
    let token = writer.authorization_reader().await.unwrap();
    let fd = token.writer.authorization.reader.file_fd();
    drop(token);
    assert!(writer.retains_authorization_reader());
    assert_eq!(writer.authorization.reader.file_fd(), fd);
    assert!(writer.authorization_reader().await.is_err());
    writer.close().await.unwrap();
    assert!(writer.authorization.reader.close_joined());
    assert!(writer.authorization_reader().await.is_err());
    drop(writer);
    pool.close().await;
}

#[tokio::test]
async fn setup_cancellation_retains_source_owner_then_terminal_workspace_cleanup() {
    let (_root, pool, mut writer) = fixture().await;
    let entered = writer
        .authorization
        .reader
        .set_setup_fault(SetupFault::Pending);
    {
        let mut acquisition = pin!(writer.authorization_reader());
        poll_fn(|cx| {
            assert!(acquisition.as_mut().poll(cx).is_pending());
            if entered.load(Ordering::Acquire) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
    }
    assert!(writer.retains_authorization_reader());
    let fd = writer.authorization.reader.file_fd();
    let opening = writer.authorization.reader.connect_address();
    assert!(writer.authorization_reader().await.is_err());
    let mut slot = RetainedAuthorizationCloseSlot::default();
    writer
        .begin_retained_authorization_close(&mut slot)
        .unwrap();
    drop(slot.join());
    assert_eq!(slot.owner.as_ref().unwrap().file_fd(), fd);
    assert_eq!(slot.owner.as_ref().unwrap().connect_address(), opening);
    slot.join().await.unwrap();
    assert!(slot.is_joined());
    writer.settle_authorization_close(&mut slot).unwrap();
    assert!(writer.authorization_reader().await.is_err());
    writer.close().await.unwrap();
    drop(slot);
    drop(writer);
    pool.close().await;
}

#[tokio::test]
async fn unpolled_inline_retirement_already_owns_real_close_and_never_reopens() {
    let (_root, pool, mut writer) = fixture().await;
    let token = writer.authorization_reader().await.unwrap();
    drop(token.retire_inline());
    assert!(writer.authorization.reader.retains_close());
    assert!(!writer.authorization.reader.close_joined());
    assert!(writer.authorization_reader().await.is_err());
    writer.close().await.unwrap();
    assert!(writer.authorization.reader.close_joined());
    assert!(writer.authorization_reader().await.is_err());
    drop(writer);
    pool.close().await;
}
