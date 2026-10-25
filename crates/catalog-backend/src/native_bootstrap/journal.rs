use super::*;
use rusqlite::ffi;
use std::ffi::CStr;
const FUNCTION: &CStr = c"motion_native_bootstrap_insert";
const TABLES: &str = "'catabolic_native_bootstrap_intents','catabolic_native_bootstrap_commands','catabolic_native_bootstrap_acks'";
struct Insert {
    kind: String,
    id: String,
    text: String,
}
unsafe extern "C" fn exact(
    ctx: *mut ffi::sqlite3_context,
    n: i32,
    args: *mut *mut ffi::sqlite3_value,
) {
    let data = unsafe { ffi::sqlite3_user_data(ctx) }.cast::<Insert>();
    let allowed = if n == 3 && !args.is_null() && !data.is_null() {
        let expected = unsafe { &*data };
        [&expected.kind, &expected.id, &expected.text]
            .iter()
            .enumerate()
            .all(|(index, text)| {
                let raw = unsafe { ffi::sqlite3_value_text(*args.add(index)) };
                !raw.is_null()
                    && unsafe { CStr::from_ptr(raw.cast()) }.to_bytes() == text.as_bytes()
            })
    } else {
        false
    };
    unsafe { ffi::sqlite3_result_int(ctx, i32::from(allowed)) };
}
unsafe extern "C" fn destroy(raw: *mut std::ffi::c_void) {
    if !raw.is_null() {
        drop(unsafe { Box::from_raw(raw.cast::<Insert>()) });
    }
}
/// Only fixed owner methods call this. Every result is revoked before returning;
/// callers consume their connection on error, so failed revocation cannot be reused.
pub(super) fn insert(db: &Connection, kind: &str, id: &str, text: &str) -> anyhow::Result<()> {
    ensure!(
        !db.is_autocommit(),
        "native41 journal requires retained writer"
    );
    let sql = match kind {
        "intent" => "INSERT INTO catabolic_native_bootstrap_intents VALUES(?,?)",
        "command" => "INSERT INTO catabolic_native_bootstrap_commands VALUES(?,?)",
        "ack" => "INSERT INTO catabolic_native_bootstrap_acks VALUES(?,?)",
        _ => anyhow::bail!("unknown native41 journal operation"),
    };
    ensure!(
        text.len() <= MAX_RECORD,
        "native41 journal record exceeds bound"
    );
    let permit = Box::into_raw(Box::new(Insert {
        kind: kind.into(),
        id: id.into(),
        text: text.into(),
    }));
    let rc = unsafe {
        ffi::sqlite3_create_function_v2(
            db.handle(),
            FUNCTION.as_ptr(),
            3,
            ffi::SQLITE_UTF8 | ffi::SQLITE_INNOCUOUS,
            permit.cast(),
            Some(exact),
            None,
            None,
            Some(destroy),
        )
    };
    ensure!(
        rc == ffi::SQLITE_OK,
        "cannot install exact native41 insertion"
    );
    let result = db.execute(sql, [id, text]);
    let revoked = unsafe {
        ffi::sqlite3_create_function_v2(
            db.handle(),
            FUNCTION.as_ptr(),
            3,
            ffi::SQLITE_UTF8,
            std::ptr::null_mut(),
            None,
            None,
            None,
            None,
        )
    };
    ensure!(
        revoked == ffi::SQLITE_OK,
        "cannot revoke native41 insertion; connection must close"
    );
    result?;
    Ok(())
}
fn schema(db: &Connection) -> anyhow::Result<Vec<(String, String, String)>> {
    let sql = format!(
        "SELECT count(*),coalesce(sum(length(CAST(sql AS BLOB))),0) FROM sqlite_schema WHERE tbl_name IN ({TABLES})"
    );
    let (count, bytes): (i64, i64) = db.query_row(&sql, [], |r| Ok((r.get(0)?, r.get(1)?)))?;
    ensure!(
        (0..=24).contains(&count) && (0..=65536).contains(&bytes),
        "native41 journal schema exceeds bound"
    );
    let mut query=db.prepare(&format!("SELECT type,name,sql FROM sqlite_schema WHERE tbl_name IN ({TABLES}) AND sql IS NOT NULL ORDER BY type,name"))?;
    Ok(query
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?)
}
pub(super) fn verify(db: &Connection) -> anyhow::Result<()> {
    let reference = Connection::open_in_memory()?;
    reference.execute_batch(include_str!("schema96.sql"))?;
    ensure!(
        schema(db)? == schema(&reference)?,
        "native41 journal schema differs"
    );
    for (table, column) in [
        ("catabolic_native_bootstrap_intents", "intent_json"),
        ("catabolic_native_bootstrap_commands", "command_json"),
        ("catabolic_native_bootstrap_acks", "proof_json"),
    ] {
        let (count, bytes): (i64, i64) = db.query_row(
            &format!(
                "SELECT count(*),coalesce(sum(length(CAST({column} AS BLOB))),0) FROM {table}"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(
            (0..=1).contains(&count) && (0..=MAX_RECORD as i64).contains(&bytes),
            "native41 journal inventory exceeds bound"
        );
    }
    ensure!(!db.query_row("SELECT EXISTS(SELECT 1 FROM catabolic_native_bootstrap_commands c LEFT JOIN catabolic_native_bootstrap_intents i USING(transition_id) WHERE i.transition_id IS NULL) OR EXISTS(SELECT 1 FROM catabolic_native_bootstrap_acks a LEFT JOIN catabolic_native_bootstrap_commands c USING(transition_id) WHERE c.transition_id IS NULL)",[],|r|r.get::<_,bool>(0))?,"native41 orphan journal record");
    Ok(())
}
pub(super) fn record(db: &Connection, kind: &str, id: &str) -> anyhow::Result<Option<String>> {
    let sql = match kind {
        "intent" => {
            "SELECT intent_json FROM catabolic_native_bootstrap_intents WHERE transition_id=?"
        }
        "command" => {
            "SELECT command_json FROM catabolic_native_bootstrap_commands WHERE transition_id=?"
        }
        "ack" => "SELECT proof_json FROM catabolic_native_bootstrap_acks WHERE transition_id=?",
        _ => anyhow::bail!("unknown native41 journal record"),
    };
    Ok(db.query_row(sql, [id], |r| r.get(0)).optional()?)
}
