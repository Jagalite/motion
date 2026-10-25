//! Exact, revocable connection-local insertion corridors. These helpers are
//! private to the relocation owner; persisted JSON cannot construct a grant.
use anyhow::ensure;
use rusqlite::ffi;
use serde_json::{Value, json};
use sqlx::SqliteConnection;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
struct Grant {
    live: Arc<AtomicBool>,
    values: Vec<Value>,
}
struct Revoke(Arc<AtomicBool>);
impl Drop for Revoke {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
unsafe extern "C" fn destroy(raw: *mut std::ffi::c_void) {
    unsafe {
        drop(Box::from_raw(raw.cast::<Grant>()));
    }
}
unsafe extern "C" fn exact(
    context: *mut ffi::sqlite3_context,
    argc: i32,
    argv: *mut *mut ffi::sqlite3_value,
) {
    let allowed = (|| {
        if argc < 0 || argv.is_null() {
            return false;
        }
        let raw = unsafe { ffi::sqlite3_user_data(context) };
        if raw.is_null() {
            return false;
        }
        let grant = unsafe { &*raw.cast::<Grant>() };
        if argc as usize != grant.values.len() || !grant.live.load(Ordering::Acquire) {
            return false;
        }
        for (i, wanted) in grant.values.iter().enumerate() {
            let arg = unsafe { *argv.add(i) };
            let actual = match unsafe { ffi::sqlite3_value_type(arg) } {
                ffi::SQLITE_INTEGER => json!(unsafe { ffi::sqlite3_value_int64(arg) }),
                ffi::SQLITE_TEXT => {
                    let len = unsafe { ffi::sqlite3_value_bytes(arg) };
                    let ptr = unsafe { ffi::sqlite3_value_text(arg) };
                    if len < 0 || ptr.is_null() {
                        return false;
                    }
                    let Ok(value) = std::str::from_utf8(unsafe {
                        std::slice::from_raw_parts(ptr, len as usize)
                    }) else {
                        return false;
                    };
                    json!(value)
                }
                _ => return false,
            };
            if &actual != wanted {
                return false;
            }
        }
        true
    })();
    unsafe {
        ffi::sqlite3_result_int(context, i32::from(allowed));
    }
}

// The guard lives across the SQL await. Cancellation revokes the exact tuple
// before the connection can be reused; a retained callback is then default-deny.
pub(super) async fn insert(
    db: &mut SqliteConnection,
    kind: Kind,
    values: Vec<Value>,
) -> anyhow::Result<()> {
    crate::source_lifecycle::require_retained_writer_in(db).await?;
    let (name, sql, count) = match kind {
        Kind::Admission => (
            c"motion_source_relocation_admission_allowed",
            "INSERT INTO catabolic_source_relocation_admissions(operation_id,source_id,catalog_id,incarnation,predecessor_json,old_mapping_json,new_mapping_json,old_mapping_digest,new_mapping_digest,requester_json,commitment_json,admission_digest,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
            13,
        ),
        Kind::Command => (
            c"motion_source_relocation_command_allowed",
            "INSERT INTO catabolic_source_relocation_commands(operation_id,command_json,command_digest,created_at) VALUES(?,?,?,?)",
            4,
        ),
        Kind::Ack => (
            c"motion_source_relocation_ack_allowed",
            "INSERT INTO catabolic_source_relocation_acks(operation_id,native_effect_json,native_effect_digest,successor_json,created_at) VALUES(?,?,?,?,?)",
            5,
        ),
    };
    ensure!(values.len() == count, "relocation insertion shape differs");
    ensure!(
        values.iter().all(|v| v.is_string() || v.as_i64().is_some()),
        "relocation insertion scalar differs"
    );
    let live = Arc::new(AtomicBool::new(true));
    let _revoke = Revoke(live.clone());
    {
        let mut handle = db.lock_handle().await?;
        let raw = Box::into_raw(Box::new(Grant {
            live,
            values: values.clone(),
        }))
        .cast();
        let code = unsafe {
            ffi::sqlite3_create_function_v2(
                handle.as_raw_handle().as_ptr(),
                name.as_ptr(),
                count as i32,
                ffi::SQLITE_UTF8 | ffi::SQLITE_INNOCUOUS,
                raw,
                Some(exact),
                None,
                None,
                Some(destroy),
            )
        };
        ensure!(
            code == ffi::SQLITE_OK,
            "cannot install exact relocation insertion owner"
        );
    }
    let mut query = sqlx::query(sql);
    for value in &values {
        query = if let Some(text) = value.as_str() {
            query.bind(text)
        } else {
            query.bind(value.as_i64().expect("validated scalar"))
        };
    }
    ensure!(
        query.execute(db).await?.rows_affected() == 1,
        "relocation insertion did not append exactly one row"
    );
    Ok(())
}
pub(super) enum Kind {
    Admission,
    Command,
    Ack,
}
