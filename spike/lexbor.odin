package spike

// Minimal lexbor bindings: parse HTML, walk the DOM, count nodes.
// Struct field offsets verified against lexbor 2.5.0 headers with gcc:
//   node_type=88 node_next=40 node_first_child=64 node_local_name=8

foreign import lxb {
	"../lexbor-2.5.0/build/liblexbor_static.a",
	"system:m",
}

Html_Document :: struct {}
Dom_Node :: struct {}

LXB_STATUS_OK              :: 0
NODE_TYPE_ELEMENT          :: 1
NODE_TYPE_TEXT             :: 3
NODE_TYPE_COMMENT          :: 8
NODE_TYPE_DOCUMENT         :: 9
NODE_OFF_TYPE              :: 88
NODE_OFF_NEXT              :: 40
NODE_OFF_FIRST_CHILD       :: 64
NODE_OFF_PARENT            :: 56
NODE_OFF_LOCAL_NAME        :: 8
LXB_TAG__TEXT              :: 2

@(default_calling_convention = "c")
foreign lxb {
	lxb_html_document_create  :: proc() -> ^Html_Document ---
	lxb_html_document_destroy :: proc(doc: ^Html_Document) -> ^Html_Document ---
	lxb_html_document_parse   :: proc(doc: ^Html_Document, html: [^]u8, size: uint) -> i32 ---
	lxb_dom_node_first_child  :: proc(node: ^Dom_Node) -> ^Dom_Node ---
	lxb_dom_node_next         :: proc(node: ^Dom_Node) -> ^Dom_Node ---
	lxb_dom_node_parent       :: proc(node: ^Dom_Node) -> ^Dom_Node ---
	lxb_dom_node_tag_id_noi       :: proc(node: ^Dom_Node) -> uint ---
	lxb_dom_node_text_content :: proc(node: ^Dom_Node, len: ^uint) -> [^]u8 ---
	lxb_tag_name_by_id_noi        :: proc(tag_id: uint, len: ^uint) -> [^]u8 ---
}

node_field :: proc(node: ^Dom_Node, off: int) -> ^Dom_Node {
	return (^ ^Dom_Node)(uintptr(node) + uintptr(off))^
}

node_u32 :: proc(node: ^Dom_Node, off: int) -> u32 {
	return (^u32)(uintptr(node) + uintptr(off))^
}

node_type :: proc(node: ^Dom_Node) -> u32 {
	return node_u32(node, NODE_OFF_TYPE)
}
