package spike

// Web storage: localStorage (SQLite, per-origin quota) and sessionStorage
// (in-memory, per context id). Synchronous get/set/remove/clear/keys.

import "core:fmt"
import "core:strings"

LOCAL_STORAGE_QUOTA :: 5 * 1024 * 1024

Local_Storage :: struct {
	st:     ^Store,
	origin: string, // "scheme://host[:port]", owned
}

local_storage_open :: proc(st: ^Store, scheme, host: string, port: int) -> Local_Storage {
	ls: Local_Storage
	ls.st = st
	if port >= 0 {
		ls.origin = fmt.aprintf("%s://%s:%d", scheme, host, port)
	} else {
		ls.origin = fmt.aprintf("%s://%s", scheme, host)
	}
	return ls
}

local_storage_close :: proc(ls: ^Local_Storage) {
	delete(ls.origin)
}

local_storage_usage :: proc(ls: ^Local_Storage) -> int {
	stmt := store_prepare(ls.st, "SELECT COALESCE(SUM(LENGTH(key)+LENGTH(value)),0) FROM localstorage WHERE origin=?")
	if stmt == nil {
		return 0
	}
	defer sqlite3_finalize(stmt)
	bind_text_copy(stmt, 1, ls.origin)
	if sqlite3_step(stmt) == SQLITE_ROW {
		return int(sqlite3_column_int64(stmt, 0))
	}
	return 0
}

local_storage_get :: proc(ls: ^Local_Storage, key: string) -> (string, bool) {
	stmt := store_prepare(ls.st, "SELECT value FROM localstorage WHERE origin=? AND key=?")
	if stmt == nil {
		return "", false
	}
	defer sqlite3_finalize(stmt)
	bind_text_copy(stmt, 1, ls.origin)
	bind_text_copy(stmt, 2, key)
	if sqlite3_step(stmt) == SQLITE_ROW {
		return col_text(stmt, 0), true
	}
	return "", false
}

// Set returns false when the write would exceed quota.
local_storage_set :: proc(ls: ^Local_Storage, key, value: string) -> bool {
	old, had := local_storage_get(ls, key)
	defer if had {
		delete(old)
	}
	delta := len(key) + len(value)
	if had {
		delta -= len(key) + len(old)
	}
	if local_storage_usage(ls) + delta > LOCAL_STORAGE_QUOTA {
		return false
	}
	stmt := store_prepare(ls.st, "INSERT OR REPLACE INTO localstorage(origin,key,value) VALUES(?,?,?)")
	if stmt == nil {
		return false
	}
	defer sqlite3_finalize(stmt)
	bind_text_copy(stmt, 1, ls.origin)
	bind_text_copy(stmt, 2, key)
	bind_text_copy(stmt, 3, value)
	return sqlite3_step(stmt) == SQLITE_DONE
}

local_storage_remove :: proc(ls: ^Local_Storage, key: string) {
	stmt := store_prepare(ls.st, "DELETE FROM localstorage WHERE origin=? AND key=?")
	if stmt == nil {
		return
	}
	defer sqlite3_finalize(stmt)
	bind_text_copy(stmt, 1, ls.origin)
	bind_text_copy(stmt, 2, key)
	sqlite3_step(stmt)
}

local_storage_clear :: proc(ls: ^Local_Storage) {
	stmt := store_prepare(ls.st, "DELETE FROM localstorage WHERE origin=?")
	if stmt == nil {
		return
	}
	defer sqlite3_finalize(stmt)
	bind_text_copy(stmt, 1, ls.origin)
	sqlite3_step(stmt)
}

local_storage_keys :: proc(ls: ^Local_Storage) -> [dynamic]string {
	out: [dynamic]string
	stmt := store_prepare(ls.st, "SELECT key FROM localstorage WHERE origin=? ORDER BY key")
	if stmt == nil {
		return out
	}
	defer sqlite3_finalize(stmt)
	bind_text_copy(stmt, 1, ls.origin)
	for sqlite3_step(stmt) == SQLITE_ROW {
		append(&out, col_text(stmt, 0))
	}
	return out
}

// Session storage: memory-only, keyed by context id (tab later).
Session_Storage :: struct {
	tabs: map[int]map[string]string, // tab id -> key -> value
}

session_storage_open :: proc() -> Session_Storage {
	return Session_Storage{make(map[int]map[string]string)}
}

session_storage_close :: proc(ss: ^Session_Storage) {
	for _, m in ss.tabs {
		for k, v in m {
			delete(k)
			delete(v)
		}
		delete(m)
	}
	delete(ss.tabs)
}

session_storage_get :: proc(ss: ^Session_Storage, ctx: int, key: string) -> (string, bool) {
	m, ok := ss.tabs[ctx]
	if !ok {
		return "", false
	}
	v, ok2 := m[key]
	if !ok2 {
		return "", false
	}
	return strings.clone(v), true
}

session_storage_set :: proc(ss: ^Session_Storage, ctx: int, key, value: string) {
	m, ok := ss.tabs[ctx]
	if !ok {
		m = make(map[string]string)
	}
	if old, has := m[key]; has {
		delete(old)
		delete_key(&m, key)
	}
	m[strings.clone(key)] = strings.clone(value)
	// NOTE: re-store: mutations through a fetched map copy do not
	// propagate to the stored header.
	ss.tabs[ctx] = m
}

session_storage_remove :: proc(ss: ^Session_Storage, ctx: int, key: string) {
	if m, ok := ss.tabs[ctx]; ok {
		if old, has := m[key]; has {
			delete(old)
			delete_key(&m, key)
			ss.tabs[ctx] = m
		}
	}
}

session_storage_clear_ctx :: proc(ss: ^Session_Storage, ctx: int) {
	if m, ok := ss.tabs[ctx]; ok {
		for k, v in m {
			delete(k)
			delete(v)
		}
		delete(m)
		delete_key(&ss.tabs, ctx)
	}
}
