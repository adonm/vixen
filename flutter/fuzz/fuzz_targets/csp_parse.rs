//! Fuzz target: `vixen_net::csp::parse_policy`.
//!
//! Property: any byte string parses into a `CspPolicy` without panicking.
//! Malformed input simply yields a (possibly empty) policy; the CSP boundary
//! (docs/SPEC.md "CSP enforcement points") must never panic on attacker-
//! controlled `Content-Security-Policy` headers.
//!
//! Run: `cargo +nightly fuzz run csp_parse -- -runs=1000000`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vixen_net::csp::parse_policy;

fuzz_target!(|data: &str| {
    let _ = parse_policy(data);
});
