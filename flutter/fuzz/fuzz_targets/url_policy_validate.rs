//! Fuzz target: `vixen_net::url_policy::validate_http_url`.
//!
//! Property: any byte string must be handled without panicking — it either
//! fails URL parsing (ignored), is rejected by policy, or is accepted. The
//! trust boundary (docs/ARCHITECTURE.md "Network fetch entry") must never
//! panic on attacker-controlled input.
//!
//! Run: `cargo +nightly fuzz run url_policy_validate -- -runs=1000000`
//! (docs/PLAN.md Phase 1 gate, docs/ACCEPTANCE.md "All fuzz targets stable
//! at 1 M iterations").

#![no_main]

use libfuzzer_sys::fuzz_target;
use vixen_net::url_policy::validate_http_url;

fuzz_target!(|data: &str| {
    // A parse failure is a valid outcome (the input wasn't a URL at all);
    // the contract under test is "no panic".
    if let Ok(url) = url::Url::parse(data) {
        let _ = validate_http_url(&url);
    }
});
