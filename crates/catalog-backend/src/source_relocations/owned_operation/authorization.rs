//! SQLite-owned sticky SQL denial on the SAME retained94 WRITE connection.
//!
//! Pinned libsqlite3-sys0.38.2 / SQLite3.53.2 public clientdata API. The reserved
//! key belongs exclusively to this concrete type. Trusted native callers must
//! not replace the connection, hooks, or this key with a foreign pointer. This
//! is a SQL restriction, not an adversarial raw-FFI/in-process sandbox.
use rusqlite::ffi;
use std::{ffi::{c_char,c_int,c_void},sync::{Arc,atomic::{AtomicBool,AtomicUsize,Ordering}}};
const KEY:&std::ffi::CStr=c"motion.service94.authorization.denial.v1";

pub(super) struct Guard { denied:Arc<AtomicBool>, bound:bool, restricted:bool }
#[repr(C)]
struct ArcAllocation {strong:AtomicUsize,weak:AtomicUsize,value:AtomicBool}
impl Guard {
    /// Current-toolchain logical Arc header/payload alignment plus both retained
    /// strong-reference handles, not a stable private-layout API guarantee.
    /// The1KiB clientdata node/key convention is a named logical node,
    /// not allocator/RSS instrumentation. Caller admits this BEFORE new().
    pub(super) const fn allocation_reservation()->u64 {
        (std::mem::size_of::<ArcAllocation>() + 2*std::mem::size_of::<Arc<AtomicBool>>() + 1024 + KEY.to_bytes_with_nul().len()) as u64
    }
    pub(super) fn new()->Self {Self {denied:Arc::new(AtomicBool::new(false)),bound:false,restricted:false}}
    pub(super) fn check_effect(&self)->anyhow::Result<()> {
        anyhow::ensure!(!self.restricted && !self.denied.load(Ordering::Acquire),"owned94 authorization denial is sticky or restriction remains");Ok(())
    }
    fn poison(&self) {self.denied.store(true,Ordering::Release);}
    fn identity(&self,handle:*mut ffi::sqlite3)->anyhow::Result<()> {
        // SAFETY: caller holds the SAME SQLx serialized handle lease. This API
        // only performs a mutex/list lookup, not SQL preparation or stepping.
        let actual=unsafe {ffi::sqlite3_get_clientdata(handle,KEY.as_ptr())};
        let expected=Arc::as_ptr(&self.denied).cast_mut().cast::<c_void>();
        if !self.bound || actual!=expected {self.poison();anyhow::bail!("owned94 authorization clientdata changed");}
        anyhow::ensure!(!self.denied.load(Ordering::Acquire),"owned94 denied SQL attempt poisoned authorization");Ok(())
    }
    pub(super) async fn restrict(&mut self,db:&mut sqlx::SqliteConnection)->anyhow::Result<()> {
        anyhow::ensure!(!self.denied.load(Ordering::Acquire),"owned94 denied SQL attempt poisoned authorization");
        // On the first poll this claim precedes the handle await and survives
        // a dropped pending waiter. An entirely unpolled async call does not
        // set it; the outer service phase already claimed the operation. No
        // owner/token address is ever passed to SQLite.
        self.restricted=true;
        let mut lease=db.lock_handle().await?;
        let handle=lease.as_raw_handle().as_ptr();
        require_write(handle)?;
        if !self.bound {
            let previous=unsafe {ffi::sqlite3_get_clientdata(handle,KEY.as_ptr())};
            if !previous.is_null(){self.poison();anyhow::bail!("owned94 reserved clientdata already occupied");}
            self.bound=true; // never reattach after removal or registration error
            let share=Arc::into_raw(Arc::clone(&self.denied)).cast_mut().cast::<c_void>();
            // SAFETY: SQLite owns this ONE strong share from this call onward.
            // On SQLITE_NOMEM SQLite invokes destroy(share) itself; never do a
            // second Arc::from_raw on this error path. No await separates bind,
            // return-code handling, identity checking and hook installation.
            let rc=unsafe {ffi::sqlite3_set_clientdata(handle,KEY.as_ptr(),share,Some(destroy))};
            if rc!=ffi::SQLITE_OK {self.poison();anyhow::bail!("owned94 clientdata registration failed");}
        }
        self.identity(handle)?;
        let rc=unsafe {ffi::sqlite3_set_authorizer(handle,Some(reads_only),handle.cast())};
        if rc!=ffi::SQLITE_OK {self.poison();anyhow::bail!("owned94 authorization hook install failed");}
        Ok(())
    }
    /// Fixed normal restoration. Same lease observes exact clientdata and the
    /// sticky flag BEFORE removing the restriction. It never clears poison.
    pub(super) async fn restore(&mut self,db:&mut sqlx::SqliteConnection)->anyhow::Result<()> {
        anyhow::ensure!(!self.denied.load(Ordering::Acquire),"owned94 denied SQL attempt poisoned authorization");
        if !self.bound && !self.restricted {return Ok(());}
        let mut lease=db.lock_handle().await?;
        let handle=lease.as_raw_handle().as_ptr();
        require_write(handle)?;
        self.identity(handle)?;
        let rc=unsafe {ffi::sqlite3_set_authorizer(handle,None,std::ptr::null_mut())};
        if rc!=ffi::SQLITE_OK {self.poison();anyhow::bail!("owned94 authorization hook restore failed");}
        self.restricted=false;Ok(())
    }
    /// Close-only restoration. Engine auto-rollback may already have ended
    /// WRITE, so this deliberately requires no transaction state and grants no
    /// effect authority. Clientdata remains owned by SQLite through real close.
    pub(super) async fn cleanup(&mut self,db:&mut sqlx::SqliteConnection)->anyhow::Result<()> {
        if !self.restricted {return Ok(());}
        let mut lease=db.lock_handle().await?;
        let handle=lease.as_raw_handle().as_ptr();
        let rc=unsafe {ffi::sqlite3_set_authorizer(handle,None,std::ptr::null_mut())};
        anyhow::ensure!(rc==ffi::SQLITE_OK,"owned94 cleanup hook removal failed");
        self.restricted=false;Ok(())
    }
    pub(super) fn restricted(&self)->bool {self.restricted}
}
fn require_write(handle:*mut ffi::sqlite3)->anyhow::Result<()> {
    anyhow::ensure!(unsafe {ffi::sqlite3_get_autocommit(handle)}==0 && unsafe {ffi::sqlite3_txn_state(handle,c"main".as_ptr())}==2,"owned94 authorization requires same actual WRITE");Ok(())
}
// No allocation, panic, SQLite calls or operation access in the destructor.
unsafe extern "C" fn destroy(pointer:*mut c_void) {
    if pointer.is_null(){return;}
    let pointer=pointer.cast::<AtomicBool>();
    unsafe {(&*pointer).store(true,Ordering::Release);drop(Arc::from_raw(pointer));}
}
unsafe extern "C" fn reads_only(db:*mut c_void,action:c_int,_:*const c_char,_:*const c_char,_:*const c_char,_:*const c_char)->c_int {
    if db.is_null(){return ffi::SQLITE_DENY;}
    // Fresh lookup avoids dangling state if SQLite removed clientdata earlier
    // in close. Supported SQLx paths serialize callbacks and clientdata updates.
    let state=unsafe {ffi::sqlite3_get_clientdata(db.cast(),KEY.as_ptr())}.cast::<AtomicBool>();
    if state.is_null(){return ffi::SQLITE_DENY;}
    let state=unsafe {&*state};
    if state.load(Ordering::Acquire){return ffi::SQLITE_DENY;}
    match action {
        ffi::SQLITE_SELECT|ffi::SQLITE_READ|ffi::SQLITE_FUNCTION=>ffi::SQLITE_OK,
        _=>{state.store(true,Ordering::Release);ffi::SQLITE_DENY}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Connection as _;
    async fn db()->sqlx::SqliteConnection {
        let mut db=sqlx::SqliteConnection::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE t (v INTEGER)").execute(&mut db).await.unwrap();
        sqlx::query("BEGIN IMMEDIATE").execute(&mut db).await.unwrap();db
    }
    #[tokio::test]
    async fn swallowed_cached_insert_denial_remains_poisoned_after_token_attempts() {
        let mut db=db().await;let mut guard=Guard::new();
        sqlx::query("INSERT INTO t VALUES(?)").bind(0i64).execute(&mut db).await.unwrap();
        guard.restrict(&mut db).await.unwrap();
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM t").fetch_one(&mut db).await.unwrap(),1);
        assert!(sqlx::query("INSERT INTO t VALUES(?)").bind(1i64).execute(&mut db).await.is_err());
        assert!(guard.restore(&mut db).await.is_err());
        assert!(guard.restrict(&mut db).await.is_err());
        assert!(guard.check_effect().is_err());
        guard.cleanup(&mut db).await.unwrap();
        sqlx::query("ROLLBACK").execute(&mut db).await.unwrap();
        assert!(guard.check_effect().is_err());
    }
    #[tokio::test]
    async fn every_effect_opcode_poison_is_sqlite_owned_until_close() {
        for sql in ["INSERT INTO t VALUES(1)","UPDATE t SET v=2","DELETE FROM t","COMMIT","ROLLBACK","PRAGMA query_only=OFF","ATTACH ':memory:' AS other"] {
            let mut db=db().await;let mut guard=Guard::new();
            guard.restrict(&mut db).await.unwrap();
            let state=Arc::clone(&guard.denied);
            assert_eq!(Arc::strong_count(&state),3);
            assert!(sqlx::query(sql).execute(&mut db).await.is_err(),"restricted SQL: {sql}");
            guard.cleanup(&mut db).await.unwrap();
            sqlx::query("ROLLBACK").execute(&mut db).await.unwrap();
            db.close().await.unwrap();
            assert!(state.load(Ordering::Acquire));assert_eq!(Arc::strong_count(&state),2);
        }
    }
    #[tokio::test]
    async fn clientdata_removal_is_not_a_new_registration_or_fresh_authority() {
        let mut db=db().await;let mut guard=Guard::new();
        guard.restrict(&mut db).await.unwrap();
        {let mut lease=db.lock_handle().await.unwrap();let raw=lease.as_raw_handle().as_ptr();
            assert_eq!(unsafe {ffi::sqlite3_set_clientdata(raw,KEY.as_ptr(),std::ptr::null_mut(),None)},ffi::SQLITE_OK);}
        assert!(sqlx::query("SELECT 1").execute(&mut db).await.is_err());
        assert!(guard.restrict(&mut db).await.is_err());assert!(guard.restore(&mut db).await.is_err());
        guard.cleanup(&mut db).await.unwrap();sqlx::query("ROLLBACK").execute(&mut db).await.unwrap();
    }
    #[tokio::test]
    async fn same_type_replacement_destroys_old_share_and_cannot_reset_owner() {
        let mut db=db().await;let mut guard=Guard::new();guard.restrict(&mut db).await.unwrap();
        let replacement=Arc::new(AtomicBool::new(false));
        {let mut lease=db.lock_handle().await.unwrap();let raw=lease.as_raw_handle().as_ptr();
            let pointer=Arc::into_raw(Arc::clone(&replacement)).cast_mut().cast();
            assert_eq!(unsafe {ffi::sqlite3_set_clientdata(raw,KEY.as_ptr(),pointer,Some(destroy))},ffi::SQLITE_OK);}
        assert!(guard.restore(&mut db).await.is_err());
        guard.cleanup(&mut db).await.unwrap();sqlx::query("ROLLBACK").execute(&mut db).await.unwrap();
        db.close().await.unwrap();assert!(replacement.load(Ordering::Acquire));assert_eq!(Arc::strong_count(&replacement),1);
    }
    #[tokio::test]
    async fn dropping_operation_does_not_free_worker_callback_state() {
        let mut db=db().await;let mut guard=Guard::new();guard.restrict(&mut db).await.unwrap();
        let state=Arc::clone(&guard.denied);drop(guard);
        assert_eq!(Arc::strong_count(&state),2);
        assert!(sqlx::query("COMMIT").execute(&mut db).await.is_err());
        // Explicit close with restriction still installed exercises close's
        // clientdata-unlink-before-final-cleanup order. No owner pointer lives.
        db.close().await.unwrap();assert!(state.load(Ordering::Acquire));assert_eq!(Arc::strong_count(&state),1);
    }
    #[tokio::test]
    async fn unpolled_installation_drops_no_sqlite_owned_share() {
        let mut db=db().await;let mut guard=Guard::new();
        drop(guard.restrict(&mut db));
        assert!(!guard.bound);assert_eq!(Arc::strong_count(&guard.denied),1);
        guard.restrict(&mut db).await.unwrap();
        assert_eq!(Arc::strong_count(&guard.denied),2);
        guard.restore(&mut db).await.unwrap();
        // Normal restoration must not remove/reset clientdata or its share.
        assert_eq!(Arc::strong_count(&guard.denied),2);
        sqlx::query("ROLLBACK").execute(&mut db).await.unwrap();
        db.close().await.unwrap();assert!(guard.denied.load(Ordering::Acquire));
        assert_eq!(Arc::strong_count(&guard.denied),1);
    }
    #[tokio::test]
    async fn cleanup_can_remove_a_claim_after_transaction_already_ended() {
        let mut db=db().await;let mut guard=Guard::new();
        guard.restrict(&mut db).await.unwrap();guard.restore(&mut db).await.unwrap();
        sqlx::query("ROLLBACK").execute(&mut db).await.unwrap();
        // Reproduce the sticky installation-claim state with an ended engine
        // transaction; do not manufacture a new authentication or native token.
        guard.restricted=true;guard.poison();
        guard.cleanup(&mut db).await.unwrap();assert!(guard.check_effect().is_err());
        db.close().await.unwrap();
    }
    #[tokio::test]
    async fn unpolled_restoration_never_removes_restriction() {
        let mut db=db().await;let mut guard=Guard::new();guard.restrict(&mut db).await.unwrap();
        drop(guard.restore(&mut db));
        assert!(sqlx::query("COMMIT").execute(&mut db).await.is_err());
        guard.cleanup(&mut db).await.unwrap();sqlx::query("ROLLBACK").execute(&mut db).await.unwrap();
    }
}
