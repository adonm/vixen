//! Fuzz target: `vixen_net::cookie::CookieJar::set_cookie`.
//!
//! Property: any `Set-Cookie` byte string is accepted/rejected without
//! panicking, against a fixed request URL. The HTTP-response → cookie-jar
//! boundary (docs/ARCHITECTURE.md) must never panic on attacker input.
//!
//! Run: `cargo +nightly fuzz run cookie_set_cookie -- -runs=1000000`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vixen_net::cookie::CookieJar;

fuzz_target!(|data: &str| {
    let request = url::Url::parse("https://example.com/a/b/c").unwrap();
    let mut jar = CookieJar::default();
    // Outcome (stored or rejected) is irrelevant; the contract is "no panic".
    let _ = jar.set_cookie(data, &request, true);
});
