# odin-spike architecture

Experimental minimal-browser spike: HTML fetch/parse, JS eval, shaped-text
render, SDL3 window, and Kitty-graphics TUI. Lives outside `flutter-dev`
on purpose — nothing here is product code until measured against a corpus.

## Pipeline

```
fetch (curl CLI, corpus/) -> parse (lexbor) -> flow layout (own)
  -> shape (kb_text_shape) -> raster (stb_truetype blits into RGBA)
  -> backends: SDL3 texture | PNG file | Kitty graphics (Ghostty)
```

One shared framebuffer feeds every backend, so all outputs agree by
construction. Diagnostics go to stderr; stdout stays clean for Kitty bytes.

## Component choices

| Layer    | Choice | Why |
|----------|--------|-----|
| Parse    | lexbor 2.5.0 static | HTML5 + CSS selectors in one C lib; ~1 MB pages in ms |
| JS       | QuickJS 2024-01-13 static | Tiny heap (~7 MB on bench), bit-identical answers to V8 on the shared workload |
| Shape    | `vendor:kb_text_shape` | Segmentation + OpenType shaping + BiDi, in-tree |
| Raster   | `vendor:stb/truetype` sources compiled in | Per-glyph-ID bitmaps (shaped IDs, not codepoints) |
| PNG      | `stb_image_write` compiled in | One call, no new dep |
| Fonts    | fontconfig discovery (dlopen, zero link deps) | no vendored files; coverage-checked by setup |
| Window   | pinned static SDL3 (`-Bstatic` fence) | shared system SDL would win the link without fencing |
| Fetch    | curl CLI (corpus pre-fetched) | In-spike fetch is future work; `vendor:curl` exists |
| Net fetch | system libcurl via own `mincurl` binding | `vendor:curl` links a nonexistent `mbedtls` lib; 15-proc surface owned instead |
| Cookies/jar | own RFC 6265 + libpsl | curl engine OFF; supercookie defense via builtin PSL |
| HTTP cache | memory LRU + SQLite index + body files | RFC 9111 subset: max-age/Expires, ETag/LM revalidation, Vary |
| Storage | SQLite (localStorage/history) + RAM (session) | one amalgamation, zero runtime deps |
| JS/WASM | QuickJS + WAMR interpreter (`libiwasm.a`) | native-call round trip proven; JS bridge is next |
| JS↔DOM | own table-driven binding DSL (`jsbind`) + lexbor | selectors/serializer collapse query/HTML; no IDL gen yet |
| Events | own capture/target/bubble dispatch | stop/remove/once; listener exceptions reported, dispatch continues |
| Browse session | heap-held store, retained lines, viewport slicing | fullscreen raster removed: 146 MB -> ~2 MB viewport |
| TUI loop | raw termios, CSI metrics, shared input buffer | typeahead survives queries; ASCII fast path |
| TUI drivers | Kitty PNG slices (Ghostty assumed, no text fallback) | GUI (`show`) manual-only until GUI suites exist |
| Forms | own fields/dataset/submit + Tab focus + cursor | per-form scoping; GET+POST; select/checkbox out of scope |
| Images | stb_image decode + stb_image_resize to display size | eager bounded pre-pass; SVG/WebP/data-URLs fall back |

SDL3 over raylib: a browser needs a platform layer (window/events/IME/GPU)
it fully owns. raylib's text story ends at unshaped TTF and its loop model
assumes games — week-one sugar against a week-three ceiling.

## Measured results (this host, 2026-09-03/04)

| Metric | spike | vixen (control) |
|--------|-------|-----------------|
| Binary | 12.4 MB, no sidecars (static SDL3; dynamic tail is libc + curl stack only) | 58.7 MB release |
| Cold-start RSS | 2.1 MB | 6.8 MB |
| app-shell page | 0.2 ms / 2.4 MB | 160 ms / 54 MB |
| JS bench, same file+answer | ~320 ms / 7 MB heap | ~230 ms / 71 MB RSS |
| Shaping suite | 8/8 (lam-alef, conjunct, BiDi, CJK uniformity) | n/a |
| Net suite (`nettest`) | 58/58 (URL, cookies, cache, Vary, redirects, storage) | n/a |
| DOM suite (`domtest`) | 20/20 (query, attrs, tree edit, events) | n/a |
| Form suite (`tuitest`) | encode/dataset/submit/layout/live GET+POST | n/a |
| Image suite (`tuitest`) | fetch/decode/resize/place/raster pixels | n/a |
| WASM round trip | add(40,2)=42, import callback=210 | n/a |

## Gotchas found (for the record)

- Upstream QuickJS uses **16-byte `JSValue` on 64-bit** (NaN boxing is
  32-bit-only). `JS_FreeValue`/`JS_IsException`/`JS_ToCString` are
  `static inline` — reachable only through `spike/shim.c`.
- kb resolves font fallback **last-pushed-first**: push the widest-coverage
  fallback first, preferred fonts last (`FONT_PATHS` order matters).
- TTC table offsets are absolute, not member-relative (`font_upm_at`).
- lexbor `lxb_dom_node_tag_id` / `lxb_tag_name_by_id` are header-inline;
  link the `_noi` twins.
- Explicit-stack DOM walk needs a separate `entered` flag: `child == nil`
  means both "unvisited" and "exhausted" (caused the title/text infinite loop).
- kb context allocator must outlive the context: heap-hold it in `Render_Ctx`.
- Never `delete` slices aliasing static tables (`FONT_PATHS[i].path`).
- Never `delete` strings borrowed from lexbor tables (`tag_name_of` clones).
- `fmt.tprintf` is temp-allocator backed: stored strings need `aprintf` —
  and must NEVER be `delete()`d. Freeing temp memory corrupts the heap
  (segfaulted the TUI via the link-hint status line). Prefer stack buffers
  (`fmt.bprintf`) or direct `fmt.printf` for transient formatting.
- Odin map headers don't propagate mutations via old copies: re-store after
  mutating nested maps; `delete_key` doesn't free key strings.
- `delete()` on dynamic arrays frees backing but leaves headers: nil anything
  reused later (browse pages), or appends write into freed memory.
- Never store pointers to own-struct fields across returns (`&sess.store`
  dangles after `browse_open` returns): heap-hold shared state.
- Double `defer` of one closer double-frees; audit teardown for duplicates.
- `JS_NewClass` references the class def (no copy): static defs with literal
  names, or typeof lies and builtins segfault.
- QuickJS setters take ownership of set values; getters/setters/methods
  installed via SetProperty must NOT be freed after install (over-release
  frees live objects). Call/property-get results MUST be freed.
- Property setters use the generic argc/argv form; a (ctx,this,val) shape
  reads the argv pointer as a value.
- `lxb_html_serialize_cb` is single-node; subtree HTML needs `tree_cb`.

## Known limitations (not bugs for spike scope)

- No mid-word break: words wider than the measure overflow the line.
- Mixed-direction lines assume kb returns runs in visual order.
- No glyph atlas: direct framebuffer blits (fine at corpus scale).
- `head`/`script`/`style` subtrees skipped; media/inputs absent.
- No JS DOM bindings; WASM has no JS bridge yet (native round trip only).
- No sandbox/fuel metering on wasm execution yet.
- Forms: text/hidden/submit/button/textarea only; select, checkbox/radio,
  file, image-button, and `type=button` skipped. Controls render even
  inside skipped landmarks (a header search box is UI, not noise).
- Images: PNG/JPEG/GIF-first-frame/BMP via stb; SVG/WebP, data: URLs,
  and srcset out of scope. 12 images / 8 MB each / 24 MB per page max.
- woff2/variable fonts untested (shipped TTFs are static instances).
- Ghostty assumed: graphics detection is environmental (`TERM` containing `ghostty`/`kitty`, or `KITTY_WINDOW_ID`); no text-driver fallback.
- Profile: `$SPIKE_PROFILE` or `~/.config/spikebrowser`; cache caps
  32 MB RAM / 256 MB disk; localStorage quota 5 MB/origin.
