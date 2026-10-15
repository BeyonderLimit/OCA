//! Phase 5 spike (ADR-0020 / D8): `sqlite-vec` must static-link into the
//! same binary as rusqlite's bundled SQLite and register as an
//! auto-extension, so Phase 11's embedding tables need no loadable
//! extension and no second database file.

use rusqlite::Connection;

/// The entry-point shape `sqlite3_auto_extension` registers (SQLite passes
/// db/err/api args the extension entry ignores — hence the transmute from
/// `sqlite_vec::sqlite3_vec_init`'s nullary extern "C" fn).
type SqliteInit = unsafe extern "C" fn(
    *mut rusqlite::ffi::sqlite3,
    *mut *mut std::os::raw::c_char,
    *const rusqlite::ffi::sqlite3_api_routines,
) -> std::os::raw::c_int;

#[test]
fn sqlite_vec_static_links_into_bundled_rusqlite() {
    // Register for every connection created after this point.
    unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<*const (), SqliteInit>(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    }

    let conn = Connection::open_in_memory().expect("connection");
    let version: String = conn
        .query_row("SELECT vec_version()", [], |row| row.get(0))
        .expect("vec_version() — the extension must be linked in-process");
    assert!(version.starts_with('v'), "vec_version: {version}");

    // The actual Phase 11 path: a vec0 table with a distance query.
    conn.execute_batch(
        "CREATE VIRTUAL TABLE vec USING vec0(embedding float[4]);
         INSERT INTO vec(rowid, embedding) VALUES (1, '[1,2,3,4]'), (2, '[9,9,9,9]');",
    )
    .expect("vec0 table");
    let nearest: i64 = conn
        .query_row(
            "SELECT rowid FROM vec WHERE embedding MATCH '[1,2,3,4]' ORDER BY distance LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("nearest neighbour");
    assert_eq!(nearest, 1, "distance order");
}
