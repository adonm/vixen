package vixen

// Images: fetch through the cache, decode with stb_image, downscale to
// display size with stb_image_resize. Bounded everywhere: capped count,
// per-image and total byte budgets, absurd-dimension clamp. Anything over
// budget (or undecodable: SVG/WebP/animated-GIF-beyond-first-frame) falls
// back to the text placeholder. data: URLs and srcset are out of scope.

import "core:strings"

MAX_PAGE_IMAGES :: 12
MAX_IMAGE_BYTES :: 8 * 1024 * 1024 // decoded RGBA cap per image
MAX_IMAGE_TOTAL :: 24 * 1024 * 1024
MAX_IMAGE_DIM   :: 2048 // clamp absurd natural dimensions early
// Natural-size gate (checked via header-only stbi_info, before any pixel
// allocation): rejects 100k×100k bombs without a 40GB malloc.
NATURAL_IMAGE_DIM :: 8192
NATURAL_IMAGE_PX  :: 16 * 1024 * 1024

// Display size from width/height attrs alone (no natural yet): fit the
// content measure, clamp absurd values, enforce the per-image byte cap.
// Used to reserve space for pending async images (no layout shift when the
// bytes arrive) and by the decoder when attrs win. Single source of truth.
image_attr_display :: proc(attr_w, attr_h, target_w: int) -> (dw, dh: int, ok: bool) {
	if attr_w <= 0 || attr_h <= 0 {
		return 0, 0, false
	}
	dw, dh = attr_w, attr_h
	for dw > MAX_IMAGE_DIM || dh > MAX_IMAGE_DIM {
		dw /= 2
		dh /= 2
	}
	if dw <= 0 || dh <= 0 {
		return 0, 0, false
	}
	if target_w > 0 && dw > target_w {
		dh = dh * target_w / dw
		dw = target_w
	}
	if dh <= 0 || dw * dh * 4 > MAX_IMAGE_BYTES {
		return 0, 0, false
	}
	return dw, dh, true
}

Image :: struct {
	url: string, // owned absolute URL
	w, h: int,  // display px (post-scale)
	aw, ah: int, // requested attr size (lookup key alongside url)
	px:   []u8, // RGBA display pixels, owned
}

Image_Placement :: struct {
	img:      int, // index into the page/rc images array
	x, y:     int, // framebuffer origin
	w, h:     int, // display px (matches image dims)
}

delete_image :: proc(im: ^Image) {
	delete(im.url)
	delete(im.px)
}

// Collect absolute http(s) img srcs + width/height attrs in tree order,
// deduplicated, capped. Owns the returned URL strings.
Image_Ref :: struct {
	url:    string, // owned absolute URL
	attr_w: int,    // width attr (0 when absent/invalid)
	attr_h: int,    // height attr (0 when absent/invalid)
}

collect_image_urls :: proc(doc: ^Html_Document, page_url: string, cap: int) -> [dynamic]Image_Ref {
	out: [dynamic]Image_Ref
	if len(page_url) == 0 {
		return out
	}
	stack: [dynamic]^Dom_Node
	defer delete(stack)
	kids: [dynamic]^Dom_Node
	defer delete(kids)
	// Seed every top-level child: starting from first-child alone strands
	// <html> whenever a doctype (or comment) comes first.
	for c := node_field((^Dom_Node)(doc), NODE_OFF_FIRST_CHILD); c != nil; c = node_field(c, NODE_OFF_NEXT) {
		append(&stack, c)
	}
	// Reverse so the first child pops first (pre-order, tree order).
	for i, j := 0, len(stack)-1; i < j; i, j = i+1, j-1 {
		stack[i], stack[j] = stack[j], stack[i]
	}
	for len(stack) > 0 {
		n := pop(&stack)
		if node_u32(n, NODE_OFF_TYPE) == NODE_TYPE_ELEMENT {
			tag := tag_name_of(n)
			defer delete(tag)
			if tag == "img" && len(out) < cap {
				if src, ok := dom_attr_val(n, "src"); ok {
					defer delete(src)
					if !strings.has_prefix(src, "data:") {
						if abs, rok := url_resolve(page_url, src); rok {
							defer delete(abs)
							aw, ah := img_attr_size(n)
							// Dedup on (url, display-size): same URL at two
							// sizes decodes twice (bounded by the page cap).
							seen := false
							for e in out {
								if e.url == abs && e.attr_w == aw && e.attr_h == ah {
									seen = true
								}
							}
							if !seen {
								append(&out, Image_Ref{strings.clone(abs), aw, ah})
							}
						}
					}
				}
			}
		}
		clear(&kids)
		for c := node_field(n, NODE_OFF_FIRST_CHILD); c != nil; c = node_field(c, NODE_OFF_NEXT) {
			append(&kids, c)
		}
		for i := len(kids) - 1; i >= 0; i -= 1 {
			append(&stack, kids[i])
		}
	}
	return out
}

// width/height attrs of an img node (0 when absent/invalid).
img_attr_size :: proc(node: ^Dom_Node) -> (w, h: int) {
	return attr_dim(node, "width"), attr_dim(node, "height")
}
// width/height attr as int; 0 when absent, invalid, or percentage.
attr_dim :: proc(node: ^Dom_Node, name: string) -> int {
	v, ok := dom_attr_val(node, name)
	if !ok {
		return 0
	}
	defer delete(v)
	t := strings.trim_space(v)
	if len(t) == 0 {
		return 0
	}
	n := 0
	for i in 0 ..< len(t) {
		c := t[i]
		if c < '0' || c > '9' {
			break
		}
		n = n * 10 + int(c - '0')
	}
	// Trailing % (or anything non-numeric past digits) invalidates.
	for i in 0 ..< len(t) {
		c := t[i]
		if (c < '0' || c > '9') && c != ' ' && c != '\t' {
			return 0
		}
	}
	if n <= 0 || n > 1 << 20 {
		return 0
	}
	return n
}
// Decode one fetched body to display-size RGBA. target_w is the content
// measure; attr_w/h (from width/height attrs, 0 when absent) set aspect.
decode_image :: proc(url: string, body: []u8, target_w, attr_w, attr_h: int) -> (Image, bool) {
	im: Image
	if len(body) == 0 || len(body) > MAX_IMAGE_BYTES * 4 {
		return im, false
	}
	// Header-only gate: refuse absurd naturals before allocating pixels.
	iw, ih, ic: i32
	if stbi_info_from_memory(raw_data(body), i32(len(body)), &iw, &ih, &ic) == 0 ||
	   iw <= 0 || ih <= 0 || iw > NATURAL_IMAGE_DIM || ih > NATURAL_IMAGE_DIM ||
	   int(iw) * int(ih) > NATURAL_IMAGE_PX {
		return im, false
	}
	nw, nh, nc: i32
	raw := stbi_load_from_memory(raw_data(body), i32(len(body)), &nw, &nh, &nc, 4)
	if raw == nil || nw <= 0 || nh <= 0 {
		return im, false
	}
	defer stbi_image_free(raw)
	// Display size: attrs win when both valid (shared helper, stable reserve
	// matches final size so attr-sized images never shift layout on arrival);
	// else natural, fitted to the target.
	dw, dh := int(nw), int(nh)
	if attr_w > 0 && attr_h > 0 {
		if aw, ah, ok := image_attr_display(attr_w, attr_h, target_w); ok {
			dw, dh = aw, ah
		} else {
			return im, false
		}
	} else {
		// Clamp absurd naturals before allocating anything.
		for dw > MAX_IMAGE_DIM || dh > MAX_IMAGE_DIM {
			dw /= 2
			dh /= 2
		}
		if dw <= 0 || dh <= 0 {
			return im, false
		}
		if target_w > 0 && dw > target_w {
			dh = dh * target_w / dw
			dw = target_w
		}
		if dh <= 0 {
			return im, false
		}
		if dw * dh * 4 > MAX_IMAGE_BYTES {
			return im, false
		}
	}
	px := make([]u8, dw * dh * 4)
	if dw == int(nw) && dh == int(nh) {
		copy(px, raw[:dw*dh*4])
	} else {
		if stbir_resize_uint8(raw, nw, nh, 0, raw_data(px), i32(dw), i32(dh), 0, 4) == 0 {
			delete(px)
			return im, false
		}
	}
	im.url = strings.clone(url)
	im.w, im.h = dw, dh
	im.aw, im.ah = attr_w, attr_h
	im.px = px
	return im, true
}

// Fetch + decode a page's images through the session cache. Owns results.
// Stops at the total byte budget; failures shrink the set, never fail it.
page_load_images :: proc(sess: ^Browse_Session, refs: []Image_Ref) -> [dynamic]Image {
	out: [dynamic]Image
	now := tnow()
	total := 0
	for r in refs {
		if len(out) >= MAX_PAGE_IMAGES || total >= MAX_IMAGE_TOTAL {
			continue // caller still owns all ref URLs
		}
		u := r.url // borrowed; freed with refs by the caller
		resp, _, ok := cached_fetch(&sess.cache, &sess.fc, &sess.jar, "GET", u, nil, nil, now)
		if !ok {
			continue
		}
		ct, _ := headers_get_first(&resp, "content-type")
		is_img := strings.has_prefix(ct, "image/")
		status_ok := resp.status == 200
		if !status_ok || !is_img {
			delete_response(&resp)
			continue
		}
		im, dok := decode_image(u, resp.body, sess.width, r.attr_w, r.attr_h)
		delete_response(&resp)
		if !dok {
			continue
		}
		total += len(im.px)
		append(&out, im)
	}
	return out
}

// Free a refs array (URLs owned).
delete_image_refs :: proc(refs: [dynamic]Image_Ref) {
	for &r in refs {
		delete(r.url)
	}
	delete(refs)
}
