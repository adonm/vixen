package spike

// Minimal SQLite C bindings (amalgamation compiled into spike/sqlite3.o).

import "core:c"

foreign import sqlite3 {"sqlite3.o", "system:pthread", "system:dl", "system:m"}

Sqlite :: struct{}

SQLITE_OK   :: 0
SQLITE_ROW  :: 100
SQLITE_DONE :: 101

SQLITE_OPEN_READWRITE :: 0x00000002
SQLITE_OPEN_CREATE    :: 0x00000004

SQLITE_TRANSIENT :: rawptr(~uintptr(0))

@(default_calling_convention = "c")
foreign sqlite3 {
	sqlite3_open_v2    :: proc(filename: cstring, ppDb: ^^Sqlite, flags: c.int, zVfs: cstring) -> c.int ---
	sqlite3_close      :: proc(db: ^Sqlite) -> c.int ---
	sqlite3_exec       :: proc(db: ^Sqlite, sql: cstring, callback: rawptr, arg: rawptr, errmsg: ^^u8) -> c.int ---
	sqlite3_free       :: proc(ptr: rawptr) ---
	sqlite3_prepare_v2 :: proc(db: ^Sqlite, zSql: cstring, nByte: c.int, ppStmt: ^^Sqlite_Stmt, pzTail: ^cstring) -> c.int ---
	sqlite3_bind_text  :: proc(stmt: ^Sqlite_Stmt, idx: c.int, text: cstring, n: c.int, dtor: rawptr) -> c.int ---
	sqlite3_bind_int64 :: proc(stmt: ^Sqlite_Stmt, idx: c.int, v: i64) -> c.int ---
	sqlite3_bind_blob  :: proc(stmt: ^Sqlite_Stmt, idx: c.int, data: rawptr, n: c.int, dtor: rawptr) -> c.int ---
	sqlite3_step       :: proc(stmt: ^Sqlite_Stmt) -> c.int ---
	sqlite3_column_text  :: proc(stmt: ^Sqlite_Stmt, iCol: c.int) -> [^]u8 ---
	sqlite3_column_bytes :: proc(stmt: ^Sqlite_Stmt, iCol: c.int) -> c.int ---
	sqlite3_column_int64 :: proc(stmt: ^Sqlite_Stmt, iCol: c.int) -> i64 ---
	sqlite3_column_blob  :: proc(stmt: ^Sqlite_Stmt, iCol: c.int) -> rawptr ---
	sqlite3_finalize   :: proc(stmt: ^Sqlite_Stmt) -> c.int ---
	sqlite3_errmsg     :: proc(db: ^Sqlite) -> cstring ---
	sqlite3_changes    :: proc(db: ^Sqlite) -> c.int ---
}

Sqlite_Stmt :: struct{}
