# `deno_core` runtime target

ADR-014 makes [`deno_core`](https://crates.io/crates/deno_core) the target JS
runtime substrate for Vixen. The Phase 2 eval gate and focused Phase 6 host smoke
checks now run behind `deno_core`.

## Migration shape

Keep the public engine seam stable:

- `vixen_engine::script::JsRuntime::new()`
- `JsRuntime::evaluate(...)`
- `JsRuntime::evaluate_with_page(...)`
- `JsValue`
- `vixen-headless --eval`
- CDP `Runtime.evaluate`

The implementation underneath is a `deno_core::JsRuntime`; host slices should
keep moving bootstrap-only surfaces into explicit feature-family extensions. Do
**not** add a generic JS-engine
trait around `deno_core`; use `deno_core` APIs directly inside
`vixen-engine::script`. The abstraction boundary is the Vixen-facing API above,
not portability to another JS engine.

## Extension layout target

Each host family should be small and explicit:

```text
crates/vixen-engine/src/script/
  runtime.rs          # deno_core runtime construction + eval bridge
  webidl.rs           # generated interface/prototype substrate
  encoding.rs         # TextEncoder/TextDecoder ops + bootstrap JS
  dom.rs              # document/Element snapshot extension + bootstrap JS
  cssom.rs            # getComputedStyle/CSS.supports/styleSheets ops + bootstrap JS
  url.rs              # URL/URLSearchParams extension
  fetch.rs            # Headers/Request/Response/Blob/File extension
```

Current state:

- `runtime.rs` owns `deno_core::JsRuntime` construction and V8 value conversion.
- `webidl.rs` renders the first generated binding substrate from a Rust-owned
  WebIDL-shaped manifest. It installs browser interface constructors/prototypes
  plus `__vixenWebidl.adoptInterface(...)`, so feature-family bootstraps attach
  concrete Vixen implementations to generated prototype chains instead of
  hand-rolling every constructor shape.
- `encoding.rs` registers the first op-backed host extension; JS constructors
  delegate UTF-8 encode/decode work to `vixen-engine::text_codec` through ops.
- `dom.rs` registers a page-snapshot extension for focused read-only
  `document`/`Element`/DOMTokenList/dataset evals. Page data crosses the
  `deno_core` op boundary through `op_vixen_dom_snapshot`; element data is
  loaded through `op_vixen_dom_element_snapshot`; selector lookup,
  `Element.matches()`, element text/attribute reads, and read-only token/dataset
  surfaces delegate through focused DOM ops. Element geometry reads
  (`getBoundingClientRect()` / `getClientRects()`) now cross a DOM rect op and
  materialize Web-shaped rect/list objects on generated WebIDL prototypes.
- `cssom.rs` registers the focused read-only CSSOM extension. `CSS.supports`,
  `getComputedStyle`, and `document.styleSheets`/CSSRule smoke data now cross
  explicit CSSOM ops and attach to generated CSSOM prototypes instead of being
  synthesized by the headless/Page string projection.
- `just gate-webidl` is the focused regression gate for this layer: generated
  interface/prototype coverage, `JsRuntime` eval, headless `--eval`, and CDP
  `Runtime.evaluate` must stay green together.

Rules:

- Rust validates near the op boundary and returns stable `EngineError` codes.
- JS bootstrap exposes Web-shaped objects but delegates behavior to Rust ops or
  shared pure modules.
- Long-lived host state uses `deno_core` resources/handles, not ad-hoc globals.
- Permissions and origin policy checks stay near the operation that crosses the
  trust boundary.

## Cache and size notes

Expect V8/`rusty_v8` artifacts to dominate JS runtime packaging. Keep Cargo and
runtime caches inside the workspace via the existing `CARGO_HOME` guidance, then
remeasure `just size-fp` before release.
