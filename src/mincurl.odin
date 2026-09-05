package vixen

// Minimal libcurl binding: only what fetch uses. Exists because
// vendor:curl's Linux foreign block links a nonexistent "mbedtls"
// library; system libcurl (OpenSSL-backed) needs no extra TLS libs.

import "core:c"

foreign import mincurl {"system:curl"}

Curl :: struct{}
CurlM :: struct{}
Curl_Slist :: struct {
	data: cstring,
	next: ^Curl_Slist,
}

// Matches libcurl's CURLMsg layout on 64-bit (msg + pad + easy + result + pad).
CurlMsg :: struct {
	msg:    c.int, // 1 = DONE
	_pad0:  c.int,
	easy:   ^Curl,
	result: c.int, // CURLcode (0 = OK)
	_pad1:  c.int,
}

CURLMSG_DONE :: 1

Curl_Code :: enum c.int {
	E_OK = 0,
}

Curl_Option :: enum c.int {
	WRITEDATA         = 10001, // CBPOINT+1
	URL               = 10002, // STRINGPOINT+2
	WRITEFUNCTION     = 20011, // FUNCTIONPOINT+11
	POSTFIELDS        = 10015, // OBJECTPOINT+15 (use POSTFIELDSIZE variant below)
	USERAGENT         = 10018, // STRINGPOINT+18
	HTTPHEADER        = 10023, // SLISTPOINT+23
	HEADERDATA        = 10029, // CBPOINT+29
	CUSTOMREQUEST     = 10036, // STRINGPOINT+36
	POSTFIELDSIZE     = 60,    // LONG+60
	NOBODY            = 44,
	FOLLOWLOCATION    = 52,
	HEADERFUNCTION    = 20079, // FUNCTIONPOINT+79
	HTTPGET           = 80,
	NOSIGNAL          = 99,
	ACCEPT_ENCODING   = 10102, // STRINGPOINT+102
	TIMEOUT_MS        = 155,
	CONNECTTIMEOUT_MS = 156,
}

Curl_Info :: enum c.int {
	RESPONSE_CODE = 0x200000 + 2,
}

@(default_calling_convention = "c", link_prefix = "curl_")
foreign mincurl {
	easy_init      :: proc() -> ^Curl ---
	easy_setopt    :: proc(curl: ^Curl, option: Curl_Option, #c_vararg args: ..any) -> Curl_Code ---
	easy_perform   :: proc(curl: ^Curl) -> Curl_Code ---
	easy_cleanup   :: proc(curl: ^Curl) ---
	easy_reset     :: proc(curl: ^Curl) ---
	easy_getinfo   :: proc(curl: ^Curl, info: Curl_Info, #c_vararg args: ..any) -> Curl_Code ---
	easy_strerror  :: proc(code: Curl_Code) -> cstring ---
	slist_append   :: proc(list: ^Curl_Slist, data: cstring) -> ^Curl_Slist ---
	slist_free_all :: proc(list: ^Curl_Slist) ---
	multi_init          :: proc() -> ^CurlM ---
	multi_add_handle    :: proc(multi: ^CurlM, easy: ^Curl) -> c.int ---
	multi_remove_handle :: proc(multi: ^CurlM, easy: ^Curl) -> c.int ---
	multi_perform       :: proc(multi: ^CurlM, running: ^c.int) -> c.int ---
	multi_info_read     :: proc(multi: ^CurlM, msgs_left: ^c.int) -> ^CurlMsg ---
	multi_cleanup       :: proc(multi: ^CurlM) -> c.int ---
}
