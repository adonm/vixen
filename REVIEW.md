# Code Quality Review

> Generated with [oy-cli](https://github.com/wagov-dtt/oy-cli): `OY_MODEL=glm-5.2 oy review . --focus 'Whole-workspace code-quality review of Rust sources (vixen-net, vixen-engine, vixen-api)'` · 2026-06-18

## Verdict

**Needs work.** The codebase is structurally strong: every reviewed module
carries spec-citing module docs, comprehensive edge-case tests, and
`#![forbid(unsafe_code)]`, and the security modules (cookie, cors, csp,
sec-fetch, sandboxing, mixed-content, referrer-policy) consistently fail
closed. However the review found **one genuine correctness bug** in
`URLSearchParams` percent-decoding, a CSP **completeness gap** (SHA-384/512
hash sources never match), and a few smaller consolidation opportunities.
None are catastrophic, but the first two should be fixed before they ship.

Scope: deterministic review of the security-sensitive `vixen-net/src`
(cookie, cors, csp, sec_fetch, sandboxing, network, referrer_policy,
mixed_content, permissions_policy) and representative `vixen-engine/src`
parsers (data_url, url_search_params), plus the public API surface
(`vixen-api/src/lib.rs`). Prose docs, fixtures, and generated/JSON manifests
were excluded as non-actionable.

## Findings summary

| # | Severity | Title | Location |
|---|----------|-------|----------|
| 1 | Medium | `URLSearchParams` mis-decodes `%2B` to space | `vixen-engine/src/url_search_params.rs:percent_decode_tf8` |
| 2 | Medium | Hand-rolled SHA-256; SHA-384/512 CSP hashes fail closed | `vixen-net/src/csp.rs:hash_matches` / `sha256` |
| 3 | Medium | Same-site heuristic comment claims fails-closed but fails open (`co.uk`) | `vixen-net/src/sec_fetch.rs:registrable_suffix` |
| 4 | Low | Two divergent hand-rolled base64 decoders across crates | `vixen-net/src/csp.rs` + `vixen-engine/src/data_url.rs` |
| 5 | Low | `Referer` full-URL serialization trims every trailing slash | `vixen-net/src/referrer_policy.rs:full_url` |
| 6 | Low | Engine dispatch test does not exercise the delegate wiring | `vixen-api/src/lib.rs` tests |
| 7 | Low | `parse_sandbox` is a repetitive 15-arm match | `vixen-net/src/sandboxing.rs:parse_sandbox` |
| 8 | Low | Response body decoded lossy, ignoring charset pipeline | `vixen-net/src/network.rs:fetch` |

## Detailed findings

### 1. `URLSearchParams` mis-decodes `%2B` to a space — Medium

**Location:** `crates/vixen-engine/src/url_search_params.rs`, function
`percent_decode_tf8`.

**Evidence:** The function first percent-decodes `%XX` runs into a byte
buffer, then — as a second pass — sweeps the *decoded* buffer replacing any
`0x2B` (`+`) with `0x20` (space):

```rust
// percent-decode loop pushes %XX bytes into `decoded` first ...
for b in decoded.iter_mut() {
    if *b == b'+' { *b = b' '; }   // applied AFTER decoding
}
```

The comment even states `+`→SPACE "happens at the byte level before decode
(URL Standard § 5.2.4)", but the code does the opposite.

**Impact:** An encoded plus `%2B` (byte `0x2B`) produced by the decode loop
is then converted to a space. So `?k=a%2Bb` parses to `"a b"` instead of
`"a+b"`. The WHATWG URL Standard § 5.2.4 replaces `+`→SPACE on the **raw**
tuple bytes *before* percent-decoding, so a literal `+` becomes a space but
`%2B` must stay a `+`. This is a spec violation that breaks the module's own
"round-trip preserves pairs" property for any value containing `+`:
`serialize` correctly emits `%2B`, but re-parsing turns it into a space. There
is no test covering `%2B` (the existing percent-decode tests only cover
`%40`, `%2f`, `%C3%A9`), which is why it slipped through.

**Design impact:** Affects `URLSearchParams`, query-string inspection, and
any form-GET round-tripping. Narrow surface (only `%2B`), but silent and
data-altering.

**Fix:** Do the `+`→SPACE replacement on the input bytes *before* decoding
(match the spec's order), e.g. translate `+` while scanning the raw bytes:
```rust
if b == b'+' { decoded.push(b' '); i += 1; continue; }
```
and drop the post-loop sweep. Add a regression test:
`assert_eq!(parse("k=%2B")[0].1, "+")` and
`assert_eq!(parse("k=a+b")[0].1, "a b")` (both in one test).

### 2. Hand-rolled SHA-256; SHA-384/512 CSP hashes fail closed — Medium

**Location:** `crates/vixen-net/src/csp.rs`, `hash_matches` and `sha256`.

**Evidence:** CSP ships a compact, dependency-free SHA-256 (with KATs for
`""` and `"abc"`) and hard-fails for the other two algorithms:

```rust
HashAlg::Sha384 | HashAlg::Sha512 => false,   // always deny
```

**Impact:** A script guarded by `'sha384-…'` or `'sha512-…'` in
`Content-Security-Policy` will **never** be authorized, so its inline script
is silently blocked. This is fail-*safe* (security), so not a hole, but it
breaks real sites that use SHA-384/512 hash sources, and the gap is
undocumented at the call site. Rolling bespoke SHA-256 — even correct — is an
ongoing audit/maintenance burden in a browser engine.

**Fix:** Adopt a vetted `sha2` crate for all three sizes (it's pure-Rust and
`unsafe`-free, compatible with the crate's `forbid(unsafe_code)`). Then
`hash_matches` becomes a uniform `sha256/384/512(content) == expected` with
the existing `constant_time_eq`.

### 3. Same-site heuristic comment claims "fails closed" but fails open — Medium

**Location:** `crates/vixen-net/src/sec_fetch.rs`, `registrable_suffix` /
`is_same_site`.

**Evidence:** The 2-label registrable-suffix heuristic is documented as
"This is correct for the common cases (`example.com`, `co.uk`-style
excepted) and **fails closed (toward cross-site)** for edge cases." But:

```rust
1 | 2 => Some(host.to_ascii_lowercase()),
_ => Some(labels[len - 2..].join(".")...),
```

For `a.co.uk` and `b.co.uk`, both yield suffix `co.uk` ⇒ `is_same_site`
returns `true` ⇒ `classify_site` returns `SameSite`. That is the **opposite**
of fail-closed: two genuinely cross-site origins are reported as same-site.

**Impact:** `Sec-Fetch-Site` can be set to `same-site` for cross-site
requests, and any downstream consumer of `SecFetchSite::is_cross_origin()`
(CORP/CORS gating) gets a permissive answer for `*.co.uk`-style domains. The
underlying limitation is acknowledged; the problem is the *safety claim* in
the doc comment is wrong, so a reviewer/greenfield consumer may trust it.

**Fix:** Either correct the comment to state the heuristic fails **open** for
multi-label public suffixes, or — before relying on this for security — gate
on a known public-suffix list (the comment already says the PSL lands when
cookie domain matching needs it; this module needs it too). At minimum,
document the failure direction truthfully.

### 4. Two divergent hand-rolled base64 decoders — Low

**Location:** `crates/vixen-net/src/csp.rs:base64_decode_or_empty` and
`crates/vixen-engine/src/data_url.rs:decode_base64_lenient` (+ `b64_val`).

**Evidence:** Two independent base64 decoders with subtly different
behaviour: CSP's decoder returns empty on any non-alphabet char and on
whitespace; data-URL's decoder skips ASCII whitespace and tolerates missing
trailing padding with careful group validation. Both are correct for their
context, but they will drift independently and double the audit surface.

**Fix:** Extract one shared, fully-tested decoder (whitespace tolerance and
padding policy as parameters, or a vetted `base64` crate) into a small shared
utility both crates can depend on.

### 5. `Referer` full-URL serialization trims every trailing slash — Low

**Location:** `crates/vixen-net/src/referrer_policy.rs`, `full_url`.

**Evidence:** `u.as_str().trim_end_matches('/').to_owned()` strips **all**
trailing slashes, not just the bare-authority root, so a source URL
`https://a.test/p/` serializes its `Referer` as `https://a.test/p` (the
trailing path slash is lost). Fetch § 4.3.7 step 6 (strip credentials +
fragment) does not call for trailing-slash trimming.

**Impact:** Minor `Referer` fidelity loss; could surprise servers that key on
trailing slashes.

**Fix:** Drop the `trim_end_matches('/')`, or restrict trimming to the
bare-authority case (`scheme://host[:port]/` → `scheme://host[:port]`).

### 6. Engine dispatch test does not exercise the delegate wiring — Low

**Location:** `crates/vixen-api/src/lib.rs`,
`trait_shape_compiles_and_dispatches`.

**Evidence:**
```rust
let mut sink = SinkDelegate::default();
engine.set_delegate(Box::new(SinkDelegate::default())); // a DIFFERENT instance
sink.title_changed("Vixen");                            // called directly
assert_eq!(sink.titles, vec!["Vixen".to_owned()]);
```
`NullEngine::set_delegate` discards the delegate, and the engine never calls
into `sink`. The assertion passes only because `title_changed` was invoked
directly on `sink` — the test would pass even if the whole engine→delegate
dispatch were broken.

**Fix:** Route the *same* `sink` through the engine (give `NullEngine` an
owned delegate it forwards to), or retitle the test to reflect that it only
checks the delegate struct in isolation.

### 7. `parse_sandbox` is a repetitive 15-arm match — Low

**Location:** `crates/vixen-net/src/sandboxing.rs`, `parse_sandbox`.

**Evidence:** Each of 15 arms repeats the identical shape
`flags = flags.union(SandboxFlags(SandboxFlags::<FLAG>))` (~60 lines of
near-duplicate code).

**Fix:** A `const TABLE: &[(&str, u32)] = &[("allow-forms", ALLOW_FORMS), …]`
plus a single `flags.0 |= bit` after a case-insensitive lookup collapses this
to ~20 lines and removes the per-arm `SandboxFlags(...)` wrapping. (The
modern `bitflags` crate is also a natural fit now that it's `unsafe`-free and
compatible with `forbid(unsafe_code)`.)

### 8. Response body decoded with lossy UTF-8, ignoring charset pipeline — Low

**Location:** `crates/vixen-net/src/network.rs`, `Network::fetch`.

**Evidence:** `let body = String::from_utf8_lossy(&bytes).into_owned();`
discards any declared `charset` and bypasses the legacy-encoding handling
that `vixen-engine::text_codec` exists to provide. Non-UTF-8 pages are silently
mangled (U+FFFD) on this path.

**Impact:** Acceptable as a documented v1.0 scope limit, but there is no
TODO/tracking note, so it's easy to forget when the charset pipeline lands.

**Fix:** At minimum leave a `// TODO(Phase 7): route through
text_codec::decode(headers, bytes)` so the handoff is explicit; ideally
thread the declared charset into the decode once `text_codec` is wired.

## Notable strengths

- Consistent `#![forbid(unsafe_code)]` across every audited module, with a
  constant-time hash comparison in CSP (`constant_time_eq`).
- Security modules fail closed throughout: cookie domain-mismatch rejection,
  SameSite=None-requires-Secure, Secure-requires-HTTPS, HttpOnly-from-script
  rejection; CORS wildcard+credentials denial; CSP `'none'`/intersection
  semantics; mixed-content active-Block; sandbox most-restrictive default.
- Cookie jar is careful with overflow (`Max-Age` `checked_add` + clamp) and
  FIFO eviction, with full round-trip and eviction tests.
- Hand-rolled base64 (data_url) and SHA-256 (csp) carry known-answer tests.
- Manual redirect loop in `network.rs` re-applies URL policy + cookies at
  every hop, with redirect-loop and body-size guards.

```oy-findings
[
  {
    "id": "USP-PLUS-DECODE",
    "severity": "medium",
    "title": "URLSearchParams mis-decodes %2B to space (+->space applied after percent-decode)",
    "location": "crates/vixen-engine/src/url_search_params.rs:percent_decode_tf8"
  },
  {
    "id": "CSP-HASH-ROLL",
    "severity": "medium",
    "title": "Hand-rolled SHA-256; SHA-384/512 CSP hash sources fail closed (inline scripts blocked)",
    "location": "crates/vixen-net/src/csp.rs:hash_matches / sha256"
  },
  {
    "id": "SECFETCH-SAMESITE",
    "severity": "medium",
    "title": "Same-site heuristic doc claims fails-closed but fails open for multi-label suffixes (co.uk)",
    "location": "crates/vixen-net/src/sec_fetch.rs:registrable_suffix / is_same_site"
  },
  {
    "id": "B64-DUP",
    "severity": "low",
    "title": "Two divergent hand-rolled base64 decoders across crates",
    "location": "crates/vixen-net/src/csp.rs:base64_decode_or_empty + crates/vixen-engine/src/data_url.rs:decode_base64_lenient"
  },
  {
    "id": "REFERER-TRIM",
    "severity": "low",
    "title": "Referer full-URL serialization trims every trailing slash from the path",
    "location": "crates/vixen-net/src/referrer_policy.rs:full_url"
  },
  {
    "id": "API-DISPATCH-TEST",
    "severity": "low",
    "title": "Engine dispatch test does not exercise the delegate wiring it claims to",
    "location": "crates/vixen-api/src/lib.rs:tests::trait_shape_compiles_and_dispatches"
  },
  {
    "id": "SANDBOX-PARSE-VERBOSE",
    "severity": "low",
    "title": "parse_sandbox is a repetitive 15-arm match that can collapse to a table",
    "location": "crates/vixen-net/src/sandboxing.rs:parse_sandbox"
  },
  {
    "id": "NET-TEXT-LOSSY",
    "severity": "low",
    "title": "Response body decoded with lossy UTF-8, ignoring the charset pipeline",
    "location": "crates/vixen-net/src/network.rs:Network::fetch"
  }
]
```
