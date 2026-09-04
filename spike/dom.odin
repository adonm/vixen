package spike

// DOM bindings: EventTarget + listeners/dispatch, Node/Element/Document/Event
// wrappers over lexbor, querySelector via lexbor selectors, (outer)HTML via
// the lexbor serializer. Single-context spike design: Dom_Ctx is reached from
// C callbacks through dom_active (documented limitation).

import "base:runtime"
import "core:fmt"
import "core:strings"

foreign import domlx {"../lexbor-2.5.0/build/liblexbor_static.a"}

Dom_Css_Parser   :: struct{}
Dom_Selectors    :: struct{}
Dom_SelectorList :: struct{}

LXB_STATUS_STOP :: 0x0013

@(default_calling_convention = "c")
foreign domlx {
	lxb_css_parser_create          :: proc() -> ^Dom_Css_Parser ---
	lxb_css_parser_init            :: proc(p: ^Dom_Css_Parser, tkz: rawptr) -> i32 ---
	lxb_css_parser_destroy         :: proc(p: ^Dom_Css_Parser, self_destroy: bool) -> rawptr ---
	lxb_selectors_create           :: proc() -> ^Dom_Selectors ---
	lxb_selectors_init             :: proc(s: ^Dom_Selectors) -> i32 ---
	lxb_selectors_destroy          :: proc(s: ^Dom_Selectors, self_destroy: bool) -> rawptr ---
	lxb_css_selectors_parse        :: proc(p: ^Dom_Css_Parser, data: [^]u8, len: uint) -> ^Dom_SelectorList ---
	lxb_css_selector_list_destroy_memory :: proc(list: ^Dom_SelectorList) ---
	lxb_selectors_find             :: proc(s: ^Dom_Selectors, root: ^Dom_Node, list: ^Dom_SelectorList, cb: rawptr, ctx: rawptr) -> i32 ---
	lxb_html_serialize_tree_cb       :: proc(node: ^Dom_Node, cb: rawptr, ctx: rawptr) -> i32 ---
	lxb_dom_element_get_attribute  :: proc(el: ^Dom_Node, name: [^]u8, nlen: uint, vlen: ^uint) -> [^]u8 ---
	lxb_dom_element_set_attribute  :: proc(el: ^Dom_Node, name: [^]u8, nlen: uint, val: [^]u8, vlen: uint) -> rawptr ---
	lxb_dom_element_remove_attribute :: proc(el: ^Dom_Node, name: [^]u8, nlen: uint) -> i32 ---
	lxb_html_document_create_element_noi :: proc(doc: ^Html_Document, name: [^]u8, nlen: uint, reserved: rawptr) -> ^Dom_Node ---
	lxb_dom_document_create_text_node :: proc(doc: ^Html_Document, data: [^]u8, len: uint) -> ^Dom_Node ---
	lxb_dom_node_insert_child      :: proc(to, node: ^Dom_Node) ---
	lxb_dom_node_remove            :: proc(node: ^Dom_Node) ---
	lxb_dom_node_insert_before     :: proc(to, node: ^Dom_Node) ---
	lxb_html_document_body_element_noi :: proc(doc: ^Html_Document) -> ^Dom_Node ---
}

// ---- event model ----

Dom_Listener :: struct {
	node:    ^Dom_Node,
	type:    string, // owned
	cb:      JS_Value, // dup'd
	capture: bool,
	once:    bool,
}

Dom_Event :: struct {
	type:     string, // owned
	target:   ^Dom_Node,
	current:  ^Dom_Node,
	stopped:  bool,
	immediate: bool,
	prevented: bool,
}

Dom_Ctx :: struct {
	rt:        ^JS_Runtime,
	ctx:       ^JS_Context,
	doc:       ^Html_Document,
	cls_node:  u32,
	cls_elem:  u32,
	cls_doc:   u32,
	cls_event: u32,
	wrappers:  map[rawptr]JS_Value, // node -> dup'd wrapper
	listeners: [dynamic]Dom_Listener,
}

dom_active: ^Dom_Ctx

// Event objects alive for finalization: wrapper bits -> event. Only
// createEvent wrappers are tracked (dispatch temporaries are not, to avoid
// map growth; stashing a dispatch arg leaks its event — noted limitation).
dom_events: map[u64]rawptr

dom_event_key :: proc(v: JS_Value) -> u64 {
	return v.u ~ u64(v.tag) ~ 0x9e3779b97f4a7c15
}

// ---- wrappers (identity-cached) ----

dom_class_for :: proc(dc: ^Dom_Ctx, node: ^Dom_Node) -> u32 {
	t := node_u32(node, NODE_OFF_TYPE)
	switch t {
	case NODE_TYPE_DOCUMENT:
		return dc.cls_doc
	case NODE_TYPE_ELEMENT:
		return dc.cls_elem
	case:
		return dc.cls_node
	}
}

dom_wrap_node :: proc(dc: ^Dom_Ctx, node: ^Dom_Node) -> JS_Value {
	if w, ok := dc.wrappers[node]; ok {
		// Fresh reference for the caller; the cache keeps its own.
		return spike_js_dup(dc.ctx, w)
	}
	class := dom_class_for(dc, node)
	w := js_wrap(dc.ctx, class, node)
	js_install_methods(dc.ctx, w, dom_node_methods())
	js_install_props(dc.ctx, w, dom_node_props())
	if class == dc.cls_elem {
		js_install_methods(dc.ctx, w, dom_el_methods())
		js_install_props(dc.ctx, w, dom_el_props())
	} else if class == dc.cls_doc {
		js_install_methods(dc.ctx, w, dom_doc_methods())
		js_install_props(dc.ctx, w, dom_doc_props())
	}
	dup := spike_js_dup(dc.ctx, w)
	dc.wrappers[node] = dup
	return w
}

// Methods shared by every node-ish wrapper (EventTarget core + tree ops).
DOM_NODE_METHODS := []Js_Method{
	{"addEventListener", dom_add_event_listener, 2},
	{"removeEventListener", dom_remove_event_listener, 2},
	{"dispatchEvent", dom_dispatch_event, 1},
	{"appendChild", dom_append_child, 1},
	{"removeChild", dom_remove_child, 1},
	{"querySelector", dom_query_selector, 1},
	{"querySelectorAll", dom_query_selector_all, 1},
}

// Methods shared by every node-ish wrapper (EventTarget core + tree ops).
dom_node_methods :: proc() -> []Js_Method {
	return DOM_NODE_METHODS
}

DOM_NODE_PROPS := []Js_Prop{
	{"nodeType", dom_node_type_get, nil},
	{"nodeName", dom_node_name_get, nil},
	{"parentNode", dom_parent_get, nil},
	{"childNodes", dom_child_nodes_get, nil},
	{"firstChild", dom_first_child_get, nil},
	{"textContent", dom_text_get, dom_text_set},
	{"ownerDocument", dom_owner_document_get, nil},
}

dom_node_props :: proc() -> []Js_Prop {
	return DOM_NODE_PROPS
}

// ---- argument helpers ----

dom_this_node :: proc(dc: ^Dom_Ctx, ctx: ^JS_Context, this: JS_Value) -> ^Dom_Node {
	if p := js_unwrap(ctx, this, dc.cls_node); p != nil {
		return (^Dom_Node)(p)
	}
	if p := js_unwrap(ctx, this, dc.cls_elem); p != nil {
		return (^Dom_Node)(p)
	}
	if p := js_unwrap(ctx, this, dc.cls_doc); p != nil {
		// Document wrapper holds the Html_Document; node is the document node.
		_ = p
		return (^Dom_Node)(dc.doc)
	}
	return nil
}

dom_arg_string :: proc(dc: ^Dom_Ctx, ctx: ^JS_Context, argc: i32, argv: [^]JS_Value, i: int) -> (string, bool) {
	_ = dc
	if int(i) >= int(argc) {
		return "", false
	}
	return js_to_string(ctx, argv[i])
}

// ---- EventTarget ----

dom_add_event_listener :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	if argc < 2 || !js_is_fn(argv[1]) {
		return js_throw(ctx, "addEventListener needs (type, listener)")
	}
	node := dom_this_node(dc, ctx, this)
	if node == nil {
		return js_throw(ctx, "addEventListener on non-node")
	}
	typ, ok := js_to_string(ctx, argv[0])
	if !ok {
		return js_throw(ctx, "addEventListener needs a string type")
	}
	defer delete(typ)
	capture := false
	if argc >= 3 && !bool(spike_js_is_undefined(argv[2])) {
		capture = js_to_bool_arg(ctx, argv[2])
	}
	append(&dc.listeners, Dom_Listener{node, strings.clone(typ), spike_js_dup(ctx, argv[1]), capture, false})
	return spike_js_undefined()
}

dom_remove_event_listener :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	if argc < 2 {
		return spike_js_undefined()
	}
	node := dom_this_node(dc, ctx, this)
	typ, ok := js_to_string(ctx, argv[0])
	if !ok || node == nil {
		if ok {
			delete(typ)
		}
		return spike_js_undefined()
	}
	defer delete(typ)
	capture := false
	if argc >= 3 && !bool(spike_js_is_undefined(argv[2])) {
		capture = js_to_bool_arg(ctx, argv[2])
	}
	for i := len(dc.listeners) - 1; i >= 0; i -= 1 {
		l := &dc.listeners[i]
		if l.node == node && l.type == typ && l.capture == capture && js_same(l.cb, argv[1]) {
			spike_js_free(ctx, l.cb)
			delete(l.type)
			ordered_remove(&dc.listeners, i)
		}
	}
	return spike_js_undefined()
}

dom_call_listener :: proc(dc: ^Dom_Ctx, l: ^Dom_Listener, target: ^Dom_Node, ev_obj: JS_Value) {
	w := dom_wrap_node(dc, target)
	defer spike_js_free(dc.ctx, w) // this-arg is borrowed; release our ref
	args := [1]JS_Value{ev_obj}
	r := JS_Call(dc.ctx, l.cb, w, 1, raw_data(args[:]))
	if spike_js_is_exception(r) != 0 {
		// Spec deviation (spike): report and continue dispatch.
		ex := JS_GetException(dc.ctx)
		defer spike_js_free(dc.ctx, ex)
		plen: uint
		cstr := spike_js_to_cstring(dc.ctx, ex, &plen)
		if cstr != nil {
			msg := string(cstr)[:plen]
			fmt.eprintfln("dom: listener threw: %s", msg)
			spike_js_free_cstring(dc.ctx, cstr)
		}
		return
	}
	spike_js_free(dc.ctx, r)
}

// Dispatch with capture/target/bubble phases. Returns !prevented.
dom_dispatch :: proc(dc: ^Dom_Ctx, target: ^Dom_Node, ev: ^Dom_Event) -> bool {
	// Propagation path target -> root.
	path: [dynamic]^Dom_Node
	defer delete(path)
	for n := target; n != nil; n = node_field(n, NODE_OFF_PARENT) {
		append(&path, n)
	}
	ev.target = target
	fire := proc(dc: ^Dom_Ctx, node: ^Dom_Node, ev: ^Dom_Event, want_capture: bool, at_target: bool) -> bool {
		// Returns false when dispatch must stop entirely.
		i := 0
		for i < len(dc.listeners) {
			l := &dc.listeners[i]
			if l.node == node && l.type == ev.type && (l.capture == want_capture || at_target) {
				ev.current = node
				obj := dom_event_object(dc, ev, false)
				dom_call_listener(dc, l, node, obj)
				spike_js_free(dc.ctx, obj)
				if l.once {
					spike_js_free(dc.ctx, l.cb)
					delete(l.type)
					ordered_remove(&dc.listeners, i)
					continue
				}
				if ev.immediate {
					return false
				}
			}
			i += 1
		}
		return !ev.stopped
	}
	// Capture: root -> target's parent.
	for i := len(path) - 1; i >= 1; i -= 1 {
		if !fire(dc, path[i], ev, true, false) {
			return !ev.prevented
		}
		if ev.stopped {
			return !ev.prevented
		}
	}
	// Target: all listeners in registration order.
	if !fire(dc, target, ev, false, true) {
		return !ev.prevented
	}
	if ev.stopped {
		return !ev.prevented
	}
	// Bubble: parent -> root.
	for i := 1; i < len(path); i += 1 {
		if !fire(dc, path[i], ev, false, false) {
			return !ev.prevented
		}
		if ev.stopped {
			return !ev.prevented
		}
	}
	return !ev.prevented
}

dom_dispatch_event :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	node := dom_this_node(dc, ctx, this)
	if argc < 1 || node == nil {
		return js_throw(ctx, "dispatchEvent needs an event")
	}
	evp := js_unwrap(ctx, argv[0], dc.cls_event)
	if evp == nil {
		return js_throw(ctx, "dispatchEvent needs an Event")
	}
	ev := (^Dom_Event)(evp)
	// Reset per-dispatch state.
	ev.stopped = false
	ev.immediate = false
	// NOTE: prevented persists across dispatches per spec (it does not reset).
	ok := dom_dispatch(dc, node, ev)
	return spike_js_new_bool(ctx, 1 if ok else 0)
}

// ---- Event object ----

dom_event_object :: proc(dc: ^Dom_Ctx, ev: ^Dom_Event, track: bool) -> JS_Value {
	obj := js_wrap(dc.ctx, dc.cls_event, ev)
	js_install_methods(dc.ctx, obj, []Js_Method{
		{"stopPropagation", dom_ev_stop, 0},
		{"stopImmediatePropagation", dom_ev_stop_immediate, 0},
		{"preventDefault", dom_ev_prevent, 0},
	})
	js_install_props(dc.ctx, obj, []Js_Prop{
		{"type", dom_ev_type_get, nil},
		{"target", dom_ev_target_get, nil},
		{"currentTarget", dom_ev_current_get, nil},
	})
	if track {
		dom_events[dom_event_key(obj)] = ev
	}
	return obj
}

dom_this_event :: proc(dc: ^Dom_Ctx, ctx: ^JS_Context, this: JS_Value) -> ^Dom_Event {
	if p := js_unwrap(ctx, this, dc.cls_event); p != nil {
		return (^Dom_Event)(p)
	}
	return nil
}

dom_ev_stop :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	if ev := dom_this_event(dom_active, ctx, this); ev != nil {
		ev.stopped = true
	}
	return spike_js_undefined()
}

dom_ev_stop_immediate :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	if ev := dom_this_event(dom_active, ctx, this); ev != nil {
		ev.stopped = true
		ev.immediate = true
	}
	return spike_js_undefined()
}

dom_ev_prevent :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	if ev := dom_this_event(dom_active, ctx, this); ev != nil {
		ev.prevented = true
	}
	return spike_js_undefined()
}

dom_ev_type_get :: proc "c" (ctx: ^JS_Context, this: JS_Value) -> JS_Value {
	context = runtime.default_context()
	if ev := dom_this_event(dom_active, ctx, this); ev != nil {
		return js_string(ctx, ev.type)
	}
	return spike_js_undefined()
}

dom_ev_target_get :: proc "c" (ctx: ^JS_Context, this: JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	if ev := dom_this_event(dc, ctx, this); ev != nil && ev.target != nil {
		return dom_wrap_node(dc, ev.target)
	}
	return spike_js_null()
}

dom_ev_current_get :: proc "c" (ctx: ^JS_Context, this: JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	if ev := dom_this_event(dc, ctx, this); ev != nil && ev.current != nil {
		return dom_wrap_node(dc, ev.current)
	}
	return spike_js_null()
}

// ---- Node props/methods ----

dom_node_type_get :: proc "c" (ctx: ^JS_Context, this: JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	node := dom_this_node(dc, ctx, this)
	if node == nil {
		return spike_js_undefined()
	}
	t := node_u32(node, NODE_OFF_TYPE)
	// DOM nodeType: element 1, text 3, comment 8, document 9.
	return spike_js_new_int32(ctx, i32(t))
}

dom_node_name_get :: proc "c" (ctx: ^JS_Context, this: JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	node := dom_this_node(dc, ctx, this)
	if node == nil {
		return spike_js_undefined()
	}
	t := node_u32(node, NODE_OFF_TYPE)
	switch t {
	case NODE_TYPE_ELEMENT:
		tag := tag_name_of(node)
		defer delete(tag)
		return js_string(ctx, strings.to_upper(tag, context.temp_allocator))
	case NODE_TYPE_TEXT:
		return js_string(ctx, "#text")
	case NODE_TYPE_COMMENT:
		return js_string(ctx, "#comment")
	case NODE_TYPE_DOCUMENT:
		return js_string(ctx, "#document")
	}
	return spike_js_undefined()
}

dom_parent_get :: proc "c" (ctx: ^JS_Context, this: JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	node := dom_this_node(dc, ctx, this)
	if node == nil {
		return spike_js_undefined()
	}
	if p := node_field(node, NODE_OFF_PARENT); p != nil {
		return dom_wrap_node(dc, p)
	}
	return spike_js_null()
}

dom_child_nodes_get :: proc "c" (ctx: ^JS_Context, this: JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	node := dom_this_node(dc, ctx, this)
	if node == nil {
		return spike_js_undefined()
	}
	arr := JS_NewArray(ctx)
	i: u32
	for c := node_field(node, NODE_OFF_FIRST_CHILD); c != nil; c = node_field(c, NODE_OFF_NEXT) {
		JS_SetPropertyUint32(ctx, arr, i, dom_wrap_node(dc, c))
		i += 1
	}
	return arr
}

dom_first_child_get :: proc "c" (ctx: ^JS_Context, this: JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	node := dom_this_node(dc, ctx, this)
	if node == nil {
		return spike_js_undefined()
	}
	if c := node_field(node, NODE_OFF_FIRST_CHILD); c != nil {
		return dom_wrap_node(dc, c)
	}
	return spike_js_null()
}

dom_text_get :: proc "c" (ctx: ^JS_Context, this: JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	_ = dc
	node := dom_this_node(dom_active, ctx, this)
	if node == nil {
		return spike_js_undefined()
	}
	tlen: uint
	ptr := lxb_dom_node_text_content(node, &tlen)
	if ptr == nil {
		return js_string(ctx, "")
	}
	return JS_NewStringLen(ctx, cstring(ptr), int(tlen))
}

dom_text_set :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	node := dom_this_node(dc, ctx, this)
	if node == nil || argc < 1 {
		return spike_js_undefined()
	}
	s, ok := js_to_string(ctx, argv[0])
	if !ok {
		return spike_js_throw_type_error(ctx, "textContent setter failed")
	}
	defer delete(s)
	// Remove all children, append one text node.
	for {
		if c := node_field(node, NODE_OFF_FIRST_CHILD); c != nil {
			lxb_dom_node_remove(c)
		} else {
			break
		}
	}
	tn := lxb_dom_document_create_text_node(dc.doc, raw_data(s), uint(len(s)))
	if tn != nil {
		lxb_dom_node_insert_child(node, tn)
	}
	return spike_js_undefined()
}

dom_owner_document_get :: proc "c" (ctx: ^JS_Context, this: JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	node := dom_this_node(dc, ctx, this)
	if node == nil {
		return spike_js_undefined()
	}
	return dom_wrap_node(dc, (^Dom_Node)(dc.doc))
}

dom_append_child :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	node := dom_this_node(dc, ctx, this)
	if argc < 1 || node == nil {
		return js_throw(ctx, "appendChild needs a node")
	}
	kid := dom_this_node(dc, ctx, argv[0])
	if kid == nil {
		return js_throw(ctx, "appendChild needs a node")
	}
	lxb_dom_node_insert_child(node, kid)
	return spike_js_dup(ctx, argv[0])
}

dom_remove_child :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	if argc < 1 {
		return js_throw(ctx, "removeChild needs a node")
	}
	kid := dom_this_node(dc, ctx, argv[0])
	if kid == nil {
		return js_throw(ctx, "removeChild needs a node")
	}
	lxb_dom_node_remove(kid)
	return spike_js_dup(ctx, argv[0])
}

// ---- Element ----

DOM_EL_METHODS := []Js_Method{
	{"getAttribute", dom_get_attr, 1},
	{"setAttribute", dom_set_attr, 2},
	{"removeAttribute", dom_remove_attr, 1},
	{"getElementsByTagName", dom_by_tag, 1},
}

dom_el_methods :: proc() -> []Js_Method {
	return DOM_EL_METHODS
}

DOM_EL_PROPS := []Js_Prop{
	{"tagName", dom_tag_get, nil},
	{"id", dom_id_get, nil},
	{"innerHTML", dom_inner_get, nil},
	{"outerHTML", dom_outer_get, nil},
}

dom_el_props :: proc() -> []Js_Prop {
	return DOM_EL_PROPS
}

dom_tag_get :: proc "c" (ctx: ^JS_Context, this: JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	node := dom_this_node(dc, ctx, this)
	if node == nil {
		return spike_js_undefined()
	}
	tag := tag_name_of(node)
	defer delete(tag)
	return js_string(ctx, strings.to_upper(tag, context.temp_allocator))
}

dom_attr_val :: proc(node: ^Dom_Node, name: string) -> (string, bool) {
	vlen: uint
	ptr := lxb_dom_element_get_attribute(node, raw_data(name), uint(len(name)), &vlen)
	if ptr == nil {
		return "", false
	}
	return strings.clone(string(cstring(ptr))[:vlen]), true
}

dom_id_get :: proc "c" (ctx: ^JS_Context, this: JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	node := dom_this_node(dc, ctx, this)
	if node == nil {
		return spike_js_undefined()
	}
	if v, ok := dom_attr_val(node, "id"); ok {
		defer delete(v)
		return js_string(ctx, v)
	}
	return js_string(ctx, "")
}

dom_get_attr :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	node := dom_this_node(dc, ctx, this)
	name, ok := dom_arg_string(dc, ctx, argc, argv, 0)
	if !ok || node == nil {
		if ok {
			delete(name)
		}
		return spike_js_null()
	}
	defer delete(name)
	if v, vok := dom_attr_val(node, strings.to_lower(name, context.temp_allocator)); vok {
		defer delete(v)
		return js_string(ctx, v)
	}
	return spike_js_null()
}

dom_set_attr :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	node := dom_this_node(dc, ctx, this)
	name, ok1 := dom_arg_string(dc, ctx, argc, argv, 0)
	val, ok2 := dom_arg_string(dc, ctx, argc, argv, 1)
	if !ok1 || !ok2 || node == nil {
		if ok1 {
			delete(name)
		}
		if ok2 {
			delete(val)
		}
		return js_throw(ctx, "setAttribute needs (name, value)")
	}
	defer delete(name)
	defer delete(val)
	lxb_dom_element_set_attribute(node, raw_data(name), uint(len(name)), raw_data(val), uint(len(val)))
	return spike_js_undefined()
}

dom_remove_attr :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	node := dom_this_node(dc, ctx, this)
	name, ok := dom_arg_string(dc, ctx, argc, argv, 0)
	if !ok || node == nil {
		if ok {
			delete(name)
		}
		return spike_js_undefined()
	}
	defer delete(name)
	lxb_dom_element_remove_attribute(node, raw_data(name), uint(len(name)))
	return spike_js_undefined()
}

dom_by_tag :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	node := dom_this_node(dc, ctx, this)
	name, ok := dom_arg_string(dc, ctx, argc, argv, 0)
	if !ok || node == nil {
		if ok {
			delete(name)
		}
		return js_throw(ctx, "getElementsByTagName needs a name")
	}
	defer delete(name)
	lower := strings.to_lower(name, context.temp_allocator)
	arr := JS_NewArray(ctx)
	i: u32
	stack: [dynamic]^Dom_Node
	defer delete(stack)
	append(&stack, node)
	for len(stack) > 0 {
		n := pop(&stack)
		if node_u32(n, NODE_OFF_TYPE) == NODE_TYPE_ELEMENT {
			tn := tag_name_of(n)
			match := tn == lower
			delete(tn)
			if match {
				JS_SetPropertyUint32(ctx, arr, i, dom_wrap_node(dc, n))
				i += 1
			}
		}
		for c := node_field(n, NODE_OFF_FIRST_CHILD); c != nil; c = node_field(c, NODE_OFF_NEXT) {
			append(&stack, c)
		}
	}
	return arr
}

// ---- querySelector(All) via lexbor selectors ----

Sel_Accum :: struct {
	nodes: [dynamic]^Dom_Node,
	alloc: runtime.Allocator,
	first_only: bool,
}

sel_find_cb :: proc "c" (node: ^Dom_Node, spec: u32, ctx: rawptr) -> i32 {
	context = {}
	_ = spec
	acc := (^Sel_Accum)(ctx)
	context.allocator = acc.alloc
	append(&acc.nodes, node)
	if acc.first_only {
		return LXB_STATUS_STOP
	}
	return 0 // LXB_STATUS_OK
}

dom_query_run :: proc(dc: ^Dom_Ctx, root: ^Dom_Node, sel: string, first_only: bool) -> ([dynamic]^Dom_Node, bool) {
	out: [dynamic]^Dom_Node
	css := lxb_css_parser_create()
	if css == nil {
		return out, false
	}
	defer lxb_css_parser_destroy(css, true)
	if lxb_css_parser_init(css, nil) != 0 {
		return out, false
	}
	selobj := lxb_selectors_create()
	if selobj == nil {
		return out, false
	}
	defer lxb_selectors_destroy(selobj, true)
	if lxb_selectors_init(selobj) != 0 {
		return out, false
	}
	list := lxb_css_selectors_parse(css, raw_data(sel), uint(len(sel)))
	if list == nil {
		return out, false
	}
	defer lxb_css_selector_list_destroy_memory(list)
	acc := Sel_Accum{nil, context.allocator, first_only}
	lxb_selectors_find(selobj, root, list, transmute(rawptr)sel_find_cb, &acc)
	out = acc.nodes
	return out, true
}

dom_query_selector :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	node := dom_this_node(dc, ctx, this)
	sel, ok := dom_arg_string(dc, ctx, argc, argv, 0)
	if !ok || node == nil {
		if ok {
			delete(sel)
		}
		return js_throw(ctx, "querySelector needs a selector")
	}
	defer delete(sel)
	found, fok := dom_query_run(dc, node, sel, true)
	defer delete(found)
	if !fok || len(found) == 0 {
		return spike_js_null()
	}
	return dom_wrap_node(dc, found[0])
}

dom_query_selector_all :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	node := dom_this_node(dc, ctx, this)
	sel, ok := dom_arg_string(dc, ctx, argc, argv, 0)
	if !ok || node == nil {
		if ok {
			delete(sel)
		}
		return js_throw(ctx, "querySelectorAll needs a selector")
	}
	defer delete(sel)
	found, fok := dom_query_run(dc, node, sel, false)
	defer delete(found)
	if !fok {
		return js_throw(ctx, "querySelectorAll failed")
	}
	arr := JS_NewArray(ctx)
	for n, i in found {
		JS_SetPropertyUint32(ctx, arr, u32(i), dom_wrap_node(dc, n))
	}
	return arr
}

// ---- (outer)HTML via the lexbor serializer ----

Ser_Accum :: struct {
	buf:   [dynamic]u8,
	alloc: runtime.Allocator,
}

ser_cb :: proc "c" (data: [^]u8, len: uint, ctx: rawptr) -> i32 {
	context = {}
	acc := (^Ser_Accum)(ctx)
	context.allocator = acc.alloc
	append(&acc.buf, ..data[:len])
	return 0
}

dom_serialize :: proc(node: ^Dom_Node) -> (string, bool) {
	acc := Ser_Accum{nil, context.allocator}
	// Serializes the node itself (outer form); callers loop children for inner.
	if lxb_html_serialize_tree_cb(node, transmute(rawptr)ser_cb, &acc) != 0 {
		delete(acc.buf)
		return "", false
	}
	s := strings.clone(string(acc.buf[:]))
	delete(acc.buf)
	return s, true
}

dom_inner_get :: proc "c" (ctx: ^JS_Context, this: JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	_ = dc
	node := dom_this_node(dom_active, ctx, this)
	if node == nil {
		return spike_js_undefined()
	}
	b: strings.Builder
	strings.builder_init(&b)
	defer delete(b.buf)
	for c := node_field(node, NODE_OFF_FIRST_CHILD); c != nil; c = node_field(c, NODE_OFF_NEXT) {
		if s, ok := dom_serialize(c); ok {
			strings.write_string(&b, s)
			delete(s)
		}
	}
	return js_string(ctx, strings.to_string(b))
}

dom_outer_get :: proc "c" (ctx: ^JS_Context, this: JS_Value) -> JS_Value {
	context = runtime.default_context()
	node := dom_this_node(dom_active, ctx, this)
	if node == nil {
		return spike_js_undefined()
	}
	if s, ok := dom_serialize(node); ok {
		defer delete(s)
		return js_string(ctx, s)
	}
	return js_string(ctx, "")
}

// ---- Document ----

DOM_DOC_METHODS := []Js_Method{
	{"addEventListener", dom_add_event_listener, 2},
	{"removeEventListener", dom_remove_event_listener, 2},
	{"dispatchEvent", dom_dispatch_event, 1},
	{"querySelector", dom_query_selector, 1},
	{"querySelectorAll", dom_query_selector_all, 1},
	{"getElementById", dom_get_by_id, 1},
	{"getElementsByTagName", dom_by_tag, 1},
	{"createElement", dom_create_element, 1},
	{"createTextNode", dom_create_text, 1},
	{"createEvent", dom_create_event, 1},
}

dom_doc_methods :: proc() -> []Js_Method {
	return DOM_DOC_METHODS
}

DOM_DOC_PROPS := []Js_Prop{
	{"documentElement", dom_doc_el_get, nil},
	{"body", dom_body_get, nil},
	{"nodeType", dom_node_type_get, nil},
	{"nodeName", dom_node_name_get, nil},
}

dom_doc_props :: proc() -> []Js_Prop {
	return DOM_DOC_PROPS
}

dom_get_by_id :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	node := dom_this_node(dc, ctx, this)
	id, ok := dom_arg_string(dc, ctx, argc, argv, 0)
	if !ok || node == nil {
		if ok {
			delete(id)
		}
		return spike_js_null()
	}
	defer delete(id)
	stack: [dynamic]^Dom_Node
	defer delete(stack)
	append(&stack, node)
	for len(stack) > 0 {
		n := pop(&stack)
		if node_u32(n, NODE_OFF_TYPE) == NODE_TYPE_ELEMENT {
			if v, vok := dom_attr_val(n, "id"); vok {
				match := v == id
				delete(v)
				if match {
					return dom_wrap_node(dc, n)
				}
			}
		}
		for c := node_field(n, NODE_OFF_FIRST_CHILD); c != nil; c = node_field(c, NODE_OFF_NEXT) {
			append(&stack, c)
		}
	}
	return spike_js_null()
}

dom_create_element :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	_ = this
	tag, ok := dom_arg_string(dc, ctx, argc, argv, 0)
	if !ok {
		return js_throw(ctx, "createElement needs a tag name")
	}
	defer delete(tag)
	el := lxb_html_document_create_element_noi(dc.doc, raw_data(tag), uint(len(tag)), nil)
	if el == nil {
		return js_throw(ctx, "createElement failed")
	}
	return dom_wrap_node(dc, el)
}

dom_create_text :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	_ = this
	s, ok := dom_arg_string(dc, ctx, argc, argv, 0)
	if !ok {
		return js_throw(ctx, "createTextNode needs text")
	}
	defer delete(s)
	tn := lxb_dom_document_create_text_node(dc.doc, raw_data(s), uint(len(s)))
	if tn == nil {
		return js_throw(ctx, "createTextNode failed")
	}
	return dom_wrap_node(dc, tn)
}

dom_create_event :: proc "c" (ctx: ^JS_Context, this: JS_Value, argc: i32, argv: [^]JS_Value) -> JS_Value {
	context = runtime.default_context()
	_ = this
	dc := dom_active
	typ, ok := dom_arg_string(dc, ctx, argc, argv, 0)
	if !ok {
		return js_throw(ctx, "createEvent needs a type")
	}
	ev := new(Dom_Event)
	ev.type = typ // ownership transferred
	return dom_event_object(dc, ev, true)
}

dom_doc_el_get :: proc "c" (ctx: ^JS_Context, this: JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	_ = this
	node := (^Dom_Node)(dc.doc)
	for c := node_field(node, NODE_OFF_FIRST_CHILD); c != nil; c = node_field(c, NODE_OFF_NEXT) {
		if node_u32(c, NODE_OFF_TYPE) == NODE_TYPE_ELEMENT {
			tn := tag_name_of(c)
			is_html := tn == "html"
			delete(tn)
			if is_html {
				return dom_wrap_node(dc, c)
			}
		}
	}
	// Fallback: first element child of document.
	for c := node_field(node, NODE_OFF_FIRST_CHILD); c != nil; c = node_field(c, NODE_OFF_NEXT) {
		if node_u32(c, NODE_OFF_TYPE) == NODE_TYPE_ELEMENT {
			return dom_wrap_node(dc, c)
		}
	}
	return spike_js_null()
}

dom_body_get :: proc "c" (ctx: ^JS_Context, this: JS_Value) -> JS_Value {
	context = runtime.default_context()
	dc := dom_active
	_ = this
	if b := lxb_html_document_body_element_noi(dc.doc); b != nil {
		return dom_wrap_node(dc, b)
	}
	return spike_js_null()
}

// ---- context lifecycle ----

// Fills a caller-owned ctx (dom_active points at it for C callbacks).
dom_context_new :: proc(html: []u8, dc: ^Dom_Ctx) -> bool {
	doc := lxb_html_document_create()
	if doc == nil {
		return false
	}
	if lxb_html_document_parse(doc, raw_data(html), uint(len(html))) != LXB_STATUS_OK {
		lxb_html_document_destroy(doc)
		return false
	}
	dc.doc = doc
	rt := JS_NewRuntime()
	if rt == nil {
		lxb_html_document_destroy(doc)
		return false
	}
	dc.rt = rt
	dc.ctx = JS_NewContext(rt)
	if dc.ctx == nil {
		lxb_html_document_destroy(doc)
		return false
	}
	dc.wrappers = make(map[rawptr]JS_Value)
	id_ok := true
	if dc.cls_node, id_ok = js_register_class(rt, &DOM_DEF_NODE); !id_ok {
		return false
	}
	if dc.cls_elem, id_ok = js_register_class(rt, &DOM_DEF_ELEM); !id_ok {
		return false
	}
	if dc.cls_doc, id_ok = js_register_class(rt, &DOM_DEF_DOC); !id_ok {
		return false
	}
	if dc.cls_event, id_ok = js_register_class(rt, &DOM_DEF_EVENT); !id_ok {
		return false
	}
	dom_active = dc
	// document global.
	docobj := js_wrap(dc.ctx, dc.cls_doc, dc.doc)
	js_install_methods(dc.ctx, docobj, dom_node_methods())
	js_install_props(dc.ctx, docobj, dom_node_props())
	js_install_methods(dc.ctx, docobj, dom_doc_methods())
	js_install_props(dc.ctx, docobj, dom_doc_props())
	js_set_global(dc.ctx, "document", docobj)
	// NOTE: SetPropertyStr consumed docobj; the global owns it now.
	// NOTE: docobj stays alive as a global property; the wrapper cache
	// intentionally does not own the document wrapper (avoid double free).
	return true
}

// The finalizer frees the Odin-side event. Event identity table entries are
// dropped here too (best effort: keyed by wrapper bits).
dom_event_finalizer_impl :: proc "c" (rt: ^JS_Runtime, v: JS_Value) {
	context = runtime.default_context()
	_ = rt
	if dom_active == nil {
		return
	}
	key := dom_event_key(v)
	if evp, ok := dom_events[key]; ok {
		ev := (^Dom_Event)(evp)
		delete(ev.type)
		free(ev)
		delete_key(&dom_events, key)
	}
}

// Static class defs (see js_register_class).
DOM_DEF_NODE := JS_Class_Def{"Node", nil, nil, nil, nil}
DOM_DEF_ELEM := JS_Class_Def{"Element", nil, nil, nil, nil}
DOM_DEF_DOC := JS_Class_Def{"Document", nil, nil, nil, nil}
DOM_DEF_EVENT := JS_Class_Def{"Event", transmute(rawptr)dom_event_finalizer_impl, nil, nil, nil}

dom_context_free :: proc(dc: ^Dom_Ctx) {
	// Free dup'd listener callbacks + cached wrappers while the ctx is live.
	for &l in dc.listeners {
		spike_js_free(dc.ctx, l.cb)
		delete(l.type)
	}
	delete(dc.listeners)
	for _, w in dc.wrappers {
		spike_js_free(dc.ctx, w)
	}
	delete(dc.wrappers)
	for _, evp in dom_events {
		ev := (^Dom_Event)(evp)
		delete(ev.type)
		free(ev)
	}
	delete(dom_events)
	JS_FreeContext(dc.ctx)
	dc.ctx = nil
	dom_active = nil
	lxb_html_document_destroy(dc.doc)
	dc.doc = nil
}

dom_eval :: proc(dc: ^Dom_Ctx, src: string) -> (string, bool) {
	v := JS_Eval(dc.ctx, strings.clone_to_cstring(src, context.temp_allocator), uint(len(src)), "<domtest>", JS_EVAL_TYPE_GLOBAL)
	defer spike_js_free(dc.ctx, v)
	if spike_js_is_exception(v) != 0 {
		ex := JS_GetException(dc.ctx)
		defer spike_js_free(dc.ctx, ex)
		plen: uint
		cstr := spike_js_to_cstring(dc.ctx, ex, &plen)
		defer spike_js_free_cstring(dc.ctx, cstr)
		if cstr == nil {
			return strings.clone(""), false
		}
		return strings.clone(string(cstr)[:plen]), false
	}
	plen: uint
	cstr := spike_js_to_cstring(dc.ctx, v, &plen)
	defer spike_js_free_cstring(dc.ctx, cstr)
	if cstr == nil {
		return strings.clone(""), false
	}
	return strings.clone(string(cstr)[:plen]), true
}
