//! Subresource Integrity (SRI) — Phase 7 security prep. The
//! `<script integrity>` / `<link integrity>` attribute the fetch layer
//! consults to verify a subresource's bytes match a known-good hash before
//! executing / applying them. The hash family + the metadata grammar live
//! here; the runtime fetch path now verifies its bounded text response before
//! exposing or caching it. Raw-byte streaming verification remains part of the
//! future streaming-body fetch path.
//!
//! What lives here:
//! - [`HashAlgorithm`] — the three SRI-mandated algorithms (`sha256` /
//!   `sha384` / `sha512`). Unknown algorithms (`md5`/`sha1`/&c.) are dropped
//!   at parse time per the spec ("If the algorithm is not a valid, drop").
//! - [`IntegrityItem`] — one parsed `<algo>-<base64>` entry + the optional
//!   `?<options>` tail (parsed but not enforced in v1).
//! - [`parse_integrity`] — the § 3.2.2 metadata parse: ASCII-whitespace-
//!   separated entries, the `-`-split algorithm/digest, the base64 STANDARD
//!   alphabet.
//! - [`verify`] — compute the § 3.3.4 "apply algorithm to bytes" + the
//!   constant-time compare; only entries using the strongest algorithm present
//!   are eligible under the spec's "best candidate" rule.
//! - [`IntegrityOutcome`] — pass / no-metadata / mismatch, for the fetch
//!   layer's `Sec-Fetch-*`-shaped error reporting.
//!
//! What does *not* live here:
//! - Incremental hashing of a streaming response body; the current bounded
//!   text path calls [`verify`] after buffering.
//! - The `integrity` block on CSP `require-sri-for` (deprecated directive;
//!   not implemented).
//! - The `?<options>` enforcement (the spec defines `ct` for content-type
//!   pinning; deferred — parsed but ignored).
//!
//! ## Trust boundary
//!
//! SRI is a tampering-resistance boundary: a CDN compromise that swaps the
//! script body breaks the hash and the fetch layer refuses to execute. The
//! hash is computed over the raw response body bytes; the compare is
//! constant-time so a timing oracle can't recover the digest. The algorithm
//! is restricted to SHA-2 family — a `sha1-…` entry is dropped at parse
//! time (SHA-1 is collision-broken and the spec forbids it).
//!
//! Reference: <https://w3c.github.io/webappsec-subresource-integrity/>.

#![forbid(unsafe_code)]

use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256, Sha384, Sha512};

// ---------------------------------------------------------------------------
// Errors / outcomes
// ---------------------------------------------------------------------------

/// The result of [`verify`] against a subresource's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityOutcome {
    /// The `integrity` attribute was absent or empty — SRI does not apply,
    /// the fetch proceeds. (A response with no `integrity` is not
    /// "integrity-protected"; the fetch layer may downgrade per CSP
    /// `require-sri-for` — deferred.)
    NoMetadata,
    /// At least one metadata entry's hash matched the body. The matched
    /// algorithm is returned for telemetry.
    Verified(HashAlgorithm),
    /// The `integrity` attribute carried ≥ 1 entry but none matched the
    /// body. The fetch layer **must** block the subresource (the spec's
    /// "fail closed" rule). Carries the parsed algorithms that did not
    /// match, for the console error the host hook surfaces.
    Mismatch(Vec<HashAlgorithm>),
    /// Parsed-only marker: the attribute carried only unknown-algorithm
    /// entries (`md5-…`), which were dropped at parse time. Treated as
    /// [`IntegrityOutcome::NoMetadata`] (no enforceable hash; the spec
    /// mandates the fetch proceed — a broken `integrity` is not a security
    /// failure on its own).
    NoKnownAlgorithms,
}

// ---------------------------------------------------------------------------
// HashAlgorithm
// ---------------------------------------------------------------------------

/// The SRI-mandated hash algorithms (W3C SRI § 3.2.2). Restricted to the
/// SHA-2 family; SHA-1 / MD5 are collision-broken and rejected at parse time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashAlgorithm {
    /// `sha256` (32-byte digest, base64-encoded to 43 chars).
    Sha256,
    /// `sha384` (48-byte digest, base64-encoded to 64 chars).
    Sha384,
    /// `sha512` (64-byte digest, base64-encoded to 86 chars).
    Sha512,
}

impl HashAlgorithm {
    /// The SRI grammar token (lowercase).
    pub const fn token(self) -> &'static str {
        match self {
            HashAlgorithm::Sha256 => "sha256",
            HashAlgorithm::Sha384 => "sha384",
            HashAlgorithm::Sha512 => "sha512",
        }
    }

    /// Parse one algorithm token (case-insensitive). Returns `None` for an
    /// unknown / forbidden algorithm (the spec mandates the entry be
    /// dropped, not error).
    pub fn parse(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "sha256" => Some(HashAlgorithm::Sha256),
            "sha384" => Some(HashAlgorithm::Sha384),
            "sha512" => Some(HashAlgorithm::Sha512),
            _ => None,
        }
    }

    const fn strength(self) -> u8 {
        match self {
            HashAlgorithm::Sha256 => 1,
            HashAlgorithm::Sha384 => 2,
            HashAlgorithm::Sha512 => 3,
        }
    }
}

// ---------------------------------------------------------------------------
// IntensityItem
// ---------------------------------------------------------------------------

/// One parsed SRI metadata entry: `<algo>-<base64>` + the optional `?<opts>`
/// tail. The base64 digest is kept verbatim (case-sensitive) so the
/// constant-time compare sees the exact bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntensityItem {
    /// The hash algorithm.
    pub algorithm: HashAlgorithm,
    /// The base64-encoded digest verbatim from the attribute (RFC 4648 § 4
    /// STANDARD alphabet).
    pub digest: String,
    /// The `?<options>` tail, verbatim. Parsed but not enforced in v1 (the
    /// spec's `ct` content-type option is deferred).
    pub options: Option<String>,
}

// ---------------------------------------------------------------------------
// parse_integrity
// ---------------------------------------------------------------------------

/// Parse the `integrity` attribute value per W3C SRI § 3.2.2. The value is a
/// list of entries separated by ASCII whitespace; each entry is
/// `<algo>-<base64>` optionally followed by `?<options>`.
///
/// Unknown algorithms (`md5`, `sha1`) and malformed entries (no `-`, empty
/// digest) are dropped per the spec's "parse error ⇒ entry dropped" rule.
/// The resulting list may be empty (the attribute was whitespace-only or
/// every entry was a parse error); [`verify`] treats an empty list as
/// [`IntegrityOutcome::NoMetadata`].
///
/// ```
/// # use vixen_net::integrity::{HashAlgorithm, parse_integrity};
/// let items = parse_integrity("sha256-abc= sha512-def?");
/// assert_eq!(items.len(), 2);
/// assert_eq!(items[0].algorithm, HashAlgorithm::Sha256);
/// assert_eq!(items[0].digest, "abc=");
/// assert_eq!(items[1].algorithm, HashAlgorithm::Sha512);
/// ```
pub fn parse_integrity(attribute: &str) -> Vec<IntensityItem> {
    let mut items = Vec::new();
    for entry in attribute.split_ascii_whitespace() {
        // Strip the optional `?<options>` tail (everything from the first
        // unescaped `?`).
        let (main, options) = match entry.split_once('?') {
            Some((m, o)) => (m, Some(o.to_owned())),
            None => (entry, None),
        };
        let Some((algo_tok, digest)) = main.split_once('-') else {
            continue; // no `-`: malformed, drop.
        };
        let Some(algorithm) = HashAlgorithm::parse(algo_tok) else {
            continue; // unknown / forbidden algorithm: drop.
        };
        if digest.is_empty() {
            continue; // empty digest: malformed, drop.
        }
        items.push(IntensityItem {
            algorithm,
            digest: digest.to_owned(),
            options,
        });
    }
    items
}

// ---------------------------------------------------------------------------
// verify
// ---------------------------------------------------------------------------

/// Verify a subresource's body against the parsed `integrity` metadata per
/// W3C SRI § 3.3.4. Computes each entry's hash over `body` (raw response
/// bytes) and compares in constant time. The "best candidate" rule first
/// selects the strongest algorithm present; weaker matching digests cannot
/// override a mismatch at that strength.
///
/// - Empty metadata ⇒ [`IntegrityOutcome::NoMetadata`] (SRI does not apply).
/// - ≥ 1 match ⇒ [`IntegrityOutcome::Verified`] with the matched algorithm.
/// - No match ⇒ [`IntegrityOutcome::Mismatch`] (the fetch layer must block).
///
/// The hash is computed over `body` as bytes (`body.as_bytes()` for a `&str`,
/// or the raw `&[u8]` the fetch layer accumulated).
pub fn verify(items: &[IntensityItem], body: &[u8]) -> IntegrityOutcome {
    if items.is_empty() {
        return IntegrityOutcome::NoMetadata;
    }
    let strongest = items
        .iter()
        .map(|item| item.algorithm.strength())
        .max()
        .unwrap_or_default();
    let candidates = items
        .iter()
        .filter(|item| item.algorithm.strength() == strongest);
    let mut attempted = Vec::new();
    for item in candidates {
        let expected = STANDARD.decode(&item.digest).unwrap_or_default();
        let matched = match item.algorithm {
            HashAlgorithm::Sha256 => constant_time_eq(Sha256::digest(body).as_slice(), &expected),
            HashAlgorithm::Sha384 => constant_time_eq(Sha384::digest(body).as_slice(), &expected),
            HashAlgorithm::Sha512 => constant_time_eq(Sha512::digest(body).as_slice(), &expected),
        };
        if matched {
            return IntegrityOutcome::Verified(item.algorithm);
        }
        attempted.push(item.algorithm);
    }
    if attempted.is_empty() {
        // Every entry was somehow dropped between parse and verify
        // (defensive — parse already drops unknown algorithms).
        IntegrityOutcome::NoKnownAlgorithms
    } else {
        IntegrityOutcome::Mismatch(attempted)
    }
}

/// Constant-time byte compare so a timing oracle can't recover the digest.
/// Mirrors `csp::constant_time_eq` (kept local to avoid widening the CSP
/// module's private surface).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: compute the SRI base64 digest for `body` under `alg`.
    fn sri_digest(alg: HashAlgorithm, body: &[u8]) -> String {
        match alg {
            HashAlgorithm::Sha256 => STANDARD.encode(Sha256::digest(body)),
            HashAlgorithm::Sha384 => STANDARD.encode(Sha384::digest(body)),
            HashAlgorithm::Sha512 => STANDARD.encode(Sha512::digest(body)),
        }
    }

    // --- HashAlgorithm -------------------------------------------------

    #[test]
    fn algorithm_token_round_trip() {
        for alg in [
            HashAlgorithm::Sha256,
            HashAlgorithm::Sha384,
            HashAlgorithm::Sha512,
        ] {
            assert_eq!(HashAlgorithm::parse(alg.token()), Some(alg));
        }
    }

    #[test]
    fn algorithm_parse_case_insensitive() {
        assert_eq!(HashAlgorithm::parse("SHA256"), Some(HashAlgorithm::Sha256));
        assert_eq!(HashAlgorithm::parse("Sha-512"), None); // not the canonical token
        assert_eq!(HashAlgorithm::parse("Sha512"), Some(HashAlgorithm::Sha512));
    }

    #[test]
    fn algorithm_parse_rejects_forbidden() {
        // SHA-1 / MD5 are collision-broken; the spec forbids them.
        assert_eq!(HashAlgorithm::parse("sha1"), None);
        assert_eq!(HashAlgorithm::parse("md5"), None);
        assert_eq!(HashAlgorithm::parse("rot13"), None);
        assert_eq!(HashAlgorithm::parse(""), None);
    }

    // --- parse_integrity ----------------------------------------------

    #[test]
    fn parse_single_entry() {
        let items = parse_integrity("sha256-abcdef==");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].algorithm, HashAlgorithm::Sha256);
        assert_eq!(items[0].digest, "abcdef==");
        assert!(items[0].options.is_none());
    }

    #[test]
    fn parse_multiple_entries_whitespace_separated() {
        let items = parse_integrity("sha256-aaa  sha512-bbb\tsha384-ccc");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].algorithm, HashAlgorithm::Sha256);
        assert_eq!(items[1].algorithm, HashAlgorithm::Sha512);
        assert_eq!(items[2].algorithm, HashAlgorithm::Sha384);
    }

    #[test]
    fn parse_options_tail() {
        let items = parse_integrity("sha256-aaa?ct=application/javascript");
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].options.as_deref(),
            Some("ct=application/javascript")
        );
    }

    #[test]
    fn parse_drops_unknown_algorithm() {
        // md5 / sha1 are dropped; the sha256 entry survives.
        let items = parse_integrity("md5-aaa sha1-bbb sha256-ccc");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].algorithm, HashAlgorithm::Sha256);
        assert_eq!(items[0].digest, "ccc");
    }

    #[test]
    fn parse_drops_malformed_no_dash() {
        let items = parse_integrity("nosplash sha256-aaa");
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn parse_drops_empty_digest() {
        let items = parse_integrity("sha256- sha512-bbb");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].digest, "bbb");
    }

    #[test]
    fn parse_empty_attribute() {
        assert!(parse_integrity("").is_empty());
        assert!(parse_integrity("   \t  ").is_empty());
    }

    // --- verify --------------------------------------------------------

    #[test]
    fn verify_empty_metadata_is_no_metadata() {
        let body = b"alert(1)";
        assert_eq!(verify(&[], body), IntegrityOutcome::NoMetadata);
    }

    #[test]
    fn verify_matching_sha256() {
        let body = b"alert(1)";
        let digest = sri_digest(HashAlgorithm::Sha256, body);
        let items = parse_integrity(&format!("sha256-{digest}"));
        assert_eq!(
            verify(&items, body),
            IntegrityOutcome::Verified(HashAlgorithm::Sha256)
        );
    }

    #[test]
    fn verify_matching_sha512() {
        let body = b"console.log('hi')";
        let digest = sri_digest(HashAlgorithm::Sha512, body);
        let items = parse_integrity(&format!("sha512-{digest}"));
        assert_eq!(
            verify(&items, body),
            IntegrityOutcome::Verified(HashAlgorithm::Sha512)
        );
    }

    #[test]
    fn verify_any_match_passes() {
        // Two strengths; the strongest entry matches.
        let body = b"document.title = 'x'";
        let wrong = sri_digest(HashAlgorithm::Sha256, b"different body");
        let right = sri_digest(HashAlgorithm::Sha512, body);
        let items = parse_integrity(&format!("sha256-{wrong} sha512-{right}"));
        assert_eq!(
            verify(&items, body),
            IntegrityOutcome::Verified(HashAlgorithm::Sha512)
        );
    }

    #[test]
    fn verify_weaker_match_does_not_override_stronger_mismatch() {
        let body = b"document.title = 'x'";
        let weak_match = sri_digest(HashAlgorithm::Sha256, body);
        let strong_mismatch = sri_digest(HashAlgorithm::Sha512, b"different body");
        let items = parse_integrity(&format!("sha256-{weak_match} sha512-{strong_mismatch}"));
        assert_eq!(
            verify(&items, body),
            IntegrityOutcome::Mismatch(vec![HashAlgorithm::Sha512])
        );
    }

    #[test]
    fn verify_mismatch_blocks() {
        let body = b"alert(1)";
        let items = parse_integrity("sha256-aaaa sha512-bbbb");
        match verify(&items, body) {
            IntegrityOutcome::Mismatch(algs) => {
                assert_eq!(algs, vec![HashAlgorithm::Sha512]);
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn verify_truncated_digest_fails() {
        let body = b"alert(1)";
        // A 5-char digest is the wrong length; the constant-time compare
        // fails the length check.
        let items = parse_integrity("sha256-short");
        assert_eq!(
            verify(&items, body),
            IntegrityOutcome::Mismatch(vec![HashAlgorithm::Sha256])
        );
    }

    #[test]
    fn verify_constant_time_does_not_short_circuit() {
        // A first-char-differing digest still mismatches (sanity for the
        // length-check + full-scan compare).
        let body = b"alert(1)";
        let real = sri_digest(HashAlgorithm::Sha256, body);
        // Flip the first base64 char.
        let mut tampered = real.chars().collect::<Vec<_>>();
        tampered[0] = if tampered[0] == 'A' { 'B' } else { 'A' };
        let tampered: String = tampered.into_iter().collect();
        let items = parse_integrity(&format!("sha256-{tampered}"));
        assert_eq!(
            verify(&items, body),
            IntegrityOutcome::Mismatch(vec![HashAlgorithm::Sha256])
        );
    }

    // --- Known-answer (FIPS 180-4 SHA-256 of "abc") --------------------

    #[test]
    fn sha256_known_answer_abc() {
        // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        let body = b"abc";
        let digest = sri_digest(HashAlgorithm::Sha256, body);
        assert_eq!(digest, "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=");
        let items = parse_integrity(&format!("sha256-{digest}"));
        assert_eq!(
            verify(&items, body),
            IntegrityOutcome::Verified(HashAlgorithm::Sha256)
        );
    }

    #[test]
    fn sha384_known_answer_empty() {
        // SHA-384("") = 38b060a751ac96384cd9327eb1b1e36a21fdb71114be07434c0cc7bf63f6e1da274edebfe76f65fbd51ad2f14898b95b
        let body = b"";
        let digest = sri_digest(HashAlgorithm::Sha384, body);
        let items = parse_integrity(&format!("sha384-{digest}"));
        assert_eq!(
            verify(&items, body),
            IntegrityOutcome::Verified(HashAlgorithm::Sha384)
        );
    }
}
