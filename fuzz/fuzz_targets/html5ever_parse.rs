//! Fuzz target: `vixen_engine::doc::Document::parse` (html5ever).
//!
//! Property: any byte string is parsed without panicking. The parse entry
//! point is the trust boundary between downloaded HTML and the engine
//! (docs/ARCHITECTURE.md); html5ever is highly permissive but must remain
//! panic-free on attacker input.
//!
//! This lives in the standalone `vixen-fuzz` workspace, so its engine
//! dependency never pulls libFuzzer or nightly requirements into normal host
//! workspace builds.
//!
//! Run: `cargo +nightly fuzz run html5ever_parse -- -runs=1000000`.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &str| {
    // Outcome (parse Ok/Err) is irrelevant; the contract is "no panic".
    // We additionally exercise the doc projections the engine runs on
    // every page (title, text, element count, dump) so a panic anywhere
    // in the read path surfaces here too.
    if let Ok(doc) = vixen_engine::doc::Document::parse(data) {
        let _ = doc.title();
        let _ = doc.text_content();
        let _ = doc.element_count();
        let _ = doc.dump();
    }
});
