package spike

// Profile store: one SQLite DB for cookies, cache index, localStorage,
// history. Bodies live as files under profile/cache/ (content-hashed names).
// Default profile: $SPIKE_PROFILE or $HOME/.config/spikebrowser.

import "core:c"
import "core:fmt"
import "core:os"
import "core:strings"
import "core:time"

Store :: struct {
	db:      ^Sqlite,
	dir:     string, // profile root (owned)
	cache_d: string, // profile/cache (owned)
}

store_profile_dir :: proc() -> string {
	if p := os.get_env("SPIKE_PROFILE", context.temp_allocator); len(p) > 0 {
		return strings.clone(p)
	}
	home := os.get_env("HOME", context.temp_allocator)
	if len(home) == 0 {
		home = "/tmp"
	}
	return fmt.aprintf("%s/.config/spikebrowser", home)
}

store_open :: proc(dir := "") -> (Store, bool) {
	st: Store
	st.dir = dir == "" ? store_profile_dir() : strings.clone(dir)
	st.cache_d = fmt.aprintf("%s/cache", st.dir)
	os.make_directory(st.dir)
	os.make_directory(st.cache_d)
	dbpath := fmt.aprintf("%s/storedb.sqlite", st.dir)
	defer delete(dbpath)
	rc := sqlite3_open_v2(strings.clone_to_cstring(dbpath, context.temp_allocator), &st.db,
		SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, nil)
	if rc != SQLITE_OK || st.db == nil {
		fmt.eprintfln("store: open failed %s", dbpath)
		return st, false
	}
	schema := string(#load("../schema.sql", string))
	if !store_exec(&st, schema) {
		return st, false
	}
	return st, true
}

store_close :: proc(st: ^Store) {
	sqlite3_close(st.db)
	st.db = nil
	delete(st.dir)
	delete(st.cache_d)
}

store_exec :: proc(st: ^Store, sql: string) -> bool {
	cs := strings.clone_to_cstring(sql, context.temp_allocator)
	em: ^u8
	rc := sqlite3_exec(st.db, cs, nil, nil, &em)
	if rc != SQLITE_OK {
		fmt.eprintfln("store: exec failed: %s", cstring(em))
		sqlite3_free(em)
		return false
	}
	return true
}

store_prepare :: proc(st: ^Store, sql: string) -> ^Sqlite_Stmt {
	cs := strings.clone_to_cstring(sql, context.temp_allocator)
	stmt: ^Sqlite_Stmt
	if sqlite3_prepare_v2(st.db, cs, -1, &stmt, nil) != SQLITE_OK {
		fmt.eprintfln("store: prepare failed: %s :: %s", sql, sqlite3_errmsg(st.db))
		return nil
	}
	return stmt
}

bind_text_copy :: proc(stmt: ^Sqlite_Stmt, idx: int, s: string) {
	cs := strings.clone_to_cstring(s, context.temp_allocator)
	sqlite3_bind_text(stmt, c.int(idx), cs, c.int(len(s)), SQLITE_TRANSIENT)
}
