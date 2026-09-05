package vixen

// Forms: field records, dataset encoding, submission building.
// Successful-control subset: text-like inputs, hidden inputs, submit
// buttons. select/textarea/file/checkbox/radio are out of scope (noted in
// ARCHITECTURE.md). Pure logic — headless tests cover it directly.

import "core:strings"

Field_Kind :: enum { text, hidden, submit }

Field :: struct {
	kind:   Field_Kind,
	name:   string, // owned; "" = unnamed (never successful)
	value:  string, // owned; current (possibly edited) value
	label:  string, // owned; button display text (submit only)
	action: string, // owned absolute form action URL ("" = no form owner)
	method: string, // owned, "GET" or "POST"
	form:   int,    // owning form id (-1 = no form owner)
	line:   int,    // laid-out line index (-1 = hidden / not rendered)
	x0:     f32,    // framebuffer x of the field box origin
	px:     f32,    // laid-out type size (cursor metrics)
}

// Form element context while walking (HTML forbids nesting; stack for leniency).
Form_Ctx :: struct {
	id:     int,    // form identity for dataset scoping
	action: string, // owned absolute URL
	method: string, // owned, "GET" or "POST"
}

delete_form_ctx :: proc(f: ^Form_Ctx) {
	delete(f.action)
	delete(f.method)
}

delete_field :: proc(f: ^Field) {
	delete(f.name)
	delete(f.value)
	delete(f.label)
	delete(f.action)
	delete(f.method)
}

// Classify an <input> type attr. Second return is false for unsupported
// types (checkbox/radio/file/select/... are out of scope).
classify_input :: proc(typ: string) -> (Field_Kind, bool) {
	switch strings.to_lower(typ, context.temp_allocator) {
	case "", "text", "search", "password", "email", "url", "tel":
		return .text, true
	case "hidden":
		return .hidden, true
	case "submit":
		return .submit, true
	}
	return .text, false
}

// Normalize a form method attr. Anything but POST is GET.
normalize_method :: proc(m: string) -> string {
	if strings.to_upper(strings.trim_space(m), context.temp_allocator) == "POST" {
		return strings.clone("POST")
	}
	return strings.clone("GET")
}

// Empty/missing action submits to the document URL minus fragment
// (url_serialize never emits fragments).
resolve_action :: proc(action_attr, page_url: string) -> (string, bool) {
	if len(page_url) == 0 {
		return "", false
	}
	if len(strings.trim_space(action_attr)) == 0 {
		u, ok := url_parse(page_url)
		if !ok {
			return "", false
		}
		defer delete_parsed_url(&u)
		return url_serialize(&u), true
	}
	return url_resolve(page_url, action_attr)
}

// application/x-www-form-urlencoded percent-encoding: unreserved bytes
// stay, space becomes '+', everything else is %HH (uppercase hex).
form_url_encode :: proc(s: string) -> string {
	b: strings.Builder
	strings.builder_init(&b, context.temp_allocator)
	hex := "0123456789ABCDEF"
	for i in 0 ..< len(s) {
		c := s[i]
		if (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
		   (c >= '0' && c <= '9') || c == '-' || c == '.' || c == '_' || c == '~' {
			strings.write_byte(&b, c)
		} else if c == ' ' {
			strings.write_byte(&b, '+')
		} else {
			strings.write_byte(&b, '%')
			strings.write_byte(&b, hex[c >> 4])
			strings.write_byte(&b, hex[c & 0xf])
		}
	}
	return strings.clone(strings.to_string(b))
}

// Dataset over the submitter's form only (same form id), in tree order,
// plus the clicked submit button when it has a name. Returns an owned
// query string (may be "").
form_dataset :: proc(fields: []Field, form_id, clicked: int) -> string {
	b: strings.Builder
	strings.builder_init(&b, context.temp_allocator)
	first := true
	emit :: proc(b: ^strings.Builder, first: ^bool, name, value: string) {
		if len(name) == 0 {
			return
		}
		if !first^ {
			strings.write_byte(b, '&')
		}
		first^ = false
		ek := form_url_encode(name)
		defer delete(ek)
		ev := form_url_encode(value)
		defer delete(ev)
		strings.write_string(b, ek)
		strings.write_byte(b, '=')
		strings.write_string(b, ev)
	}
	for &f, i in fields {
		if f.form != form_id {
			continue
		}
		switch f.kind {
		case .text, .hidden:
			emit(&b, &first, f.name, f.value)
		case .submit:
			if i == clicked {
				emit(&b, &first, f.name, f.value)
			}
		}
	}
	return strings.clone(strings.to_string(b))
}

Form_Request :: struct {
	method: string, // owned, "GET" or "POST"
	url:    string, // owned absolute URL
	body:   string, // owned; "" for GET
}

delete_form_request :: proc(r: ^Form_Request) {
	delete(r.method)
	delete(r.url)
	delete(r.body)
}

// Build the submission for the clicked control (submit button) or -1 when
// submitted from a text field (no button). Only controls sharing the
// submitter's form id contribute. The action URL is parsed, the query
// replaced (GET) or kept (POST, body carries it).
form_submit :: proc(action, method: string, fields: []Field, form_id, clicked: int) -> (Form_Request, bool) {
	r: Form_Request
	m := "POST" if strings.to_upper(method, context.temp_allocator) == "POST" else "GET"
	u, ok := url_parse(action)
	if !ok {
		return r, false
	}
	defer delete_parsed_url(&u)
	data := form_dataset(fields, form_id, clicked)
	defer delete(data)
	if m == "GET" {
		delete(u.query)
		u.query = strings.clone(data)
		r.method = strings.clone("GET")
		r.url = url_serialize(&u) // serialize drops the fragment
		r.body = strings.clone("")
	} else {
		r.method = strings.clone("POST")
		r.url = url_serialize(&u) // action as-is (fragment dropped)
		r.body = strings.clone(data)
	}
	return r, true
}
