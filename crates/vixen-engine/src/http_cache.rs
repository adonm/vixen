//! Bounded private HTTP-cache decisions shared by BrowserCore resource loads
//! and page `fetch()`/XHR.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashSet};

use vixen_store::CacheEntry;

const MAX_VARY_FIELDS: usize = 32;
const MAX_VARY_NAME_BYTES: usize = 256;
const MAX_VARY_VALUES_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheUse {
    Fresh,
    Stale,
    Unusable,
}

pub(crate) fn cache_use(
    entry: &CacheEntry,
    request_headers: &BTreeMap<String, String>,
    now_unix: i64,
    max_body_bytes: u64,
) -> CacheUse {
    if !(200..300).contains(&entry.status)
        || entry.body.len() as u64 > max_body_bytes
        || cache_control_has(&entry.headers, "no-store")
        || !vary_matches(entry, request_headers)
    {
        return CacheUse::Unusable;
    }
    if cache_control_has(&entry.headers, "no-cache") {
        return CacheUse::Stale;
    }
    let Some(max_age) = cache_control_seconds(&entry.headers, "max-age") else {
        return CacheUse::Stale;
    };
    let age = header(&entry.headers, "age")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or_default();
    let resident = now_unix.saturating_sub(entry.fetched_unix).max(0) as u64;
    if age.saturating_add(resident) < max_age {
        CacheUse::Fresh
    } else {
        CacheUse::Stale
    }
}

pub(crate) fn cache_entry(
    status: u16,
    headers: &BTreeMap<String, String>,
    body: Vec<u8>,
    request_headers: &BTreeMap<String, String>,
    fetched_unix: i64,
) -> Option<CacheEntry> {
    if !(200..300).contains(&status) || cache_control_has_map(headers, "no-store") {
        return None;
    }
    let vary_headers = capture_vary(headers.get("vary").map(String::as_str), request_headers)?;
    Some(CacheEntry {
        status,
        headers: headers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
        body,
        fetched_unix,
        vary_headers,
    })
}

pub(crate) fn has_validator(entry: &CacheEntry) -> bool {
    header(&entry.headers, "etag").is_some() || header(&entry.headers, "last-modified").is_some()
}

pub(crate) fn select_variant(
    entries: Vec<CacheEntry>,
    request_headers: &BTreeMap<String, String>,
    now_unix: i64,
    max_body_bytes: u64,
) -> Option<(CacheEntry, CacheUse)> {
    entries
        .into_iter()
        .filter_map(|entry| {
            let cache_use = cache_use(&entry, request_headers, now_unix, max_body_bytes);
            (cache_use != CacheUse::Unusable).then_some((entry, cache_use))
        })
        .max_by_key(|(entry, cache_use)| {
            let usable_rank = match cache_use {
                CacheUse::Fresh => 1,
                CacheUse::Stale | CacheUse::Unusable => 0,
            };
            (entry.fetched_unix, usable_rank)
        })
}

fn vary_matches(entry: &CacheEntry, request_headers: &BTreeMap<String, String>) -> bool {
    let Some(names) = vary_names(header(&entry.headers, "vary")) else {
        return false;
    };
    if names.is_empty() {
        return entry.vary_headers.is_empty();
    }
    if entry.vary_headers.len() != names.len() {
        return false;
    }
    names.into_iter().all(|name| {
        entry
            .vary_headers
            .iter()
            .find(|(stored, _)| stored == &name)
            .is_some_and(|(_, value)| value.as_ref() == request_headers.get(&name))
    })
}

fn capture_vary(
    value: Option<&str>,
    request_headers: &BTreeMap<String, String>,
) -> Option<Vec<(String, Option<String>)>> {
    let names = vary_names(value)?;
    let mut total_value_bytes = 0_usize;
    let mut captured = Vec::with_capacity(names.len());
    for name in names {
        let value = request_headers.get(&name).cloned();
        total_value_bytes = total_value_bytes.checked_add(value.as_ref().map_or(0, String::len))?;
        if total_value_bytes > MAX_VARY_VALUES_BYTES {
            return None;
        }
        captured.push((name, value));
    }
    Some(captured)
}

fn vary_names(value: Option<&str>) -> Option<Vec<String>> {
    let Some(value) = value else {
        return Some(Vec::new());
    };
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for field in value.split(',') {
        let name = field.trim().to_ascii_lowercase();
        if name == "*" || !valid_header_name(&name) {
            return None;
        }
        if seen.insert(name.clone()) {
            if names.len() >= MAX_VARY_FIELDS {
                return None;
            }
            names.push(name);
        }
    }
    Some(names)
}

fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_VARY_NAME_BYTES
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn cache_control_has(headers: &[(String, String)], wanted: &str) -> bool {
    header(headers, "cache-control").is_some_and(|value| directive(value, wanted).is_some())
}

fn cache_control_has_map(headers: &BTreeMap<String, String>, wanted: &str) -> bool {
    headers
        .get("cache-control")
        .is_some_and(|value| directive(value, wanted).is_some())
}

fn cache_control_seconds(headers: &[(String, String)], wanted: &str) -> Option<u64> {
    directive(header(headers, "cache-control")?, wanted)?
        .and_then(|value| value.trim_matches('"').parse().ok())
}

fn directive<'a>(value: &'a str, wanted: &str) -> Option<Option<&'a str>> {
    value.split(',').find_map(|item| {
        let (name, value) = item
            .split_once('=')
            .map_or((item.trim(), None), |(name, value)| {
                (name.trim(), Some(value.trim()))
            });
        name.eq_ignore_ascii_case(wanted).then_some(value)
    })
}

fn header<'a>(headers: &'a [(String, String)], wanted: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
        .map(|(_, value)| value.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(headers: &[(&str, &str)], vary_headers: Vec<(String, Option<String>)>) -> CacheEntry {
        CacheEntry {
            status: 200,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            body: b"body".to_vec(),
            fetched_unix: 100,
            vary_headers,
        }
    }

    #[test]
    fn max_age_and_age_bound_freshness() {
        let request = BTreeMap::new();
        let cached = entry(
            &[("cache-control", "private, max-age=60"), ("age", "10")],
            Vec::new(),
        );
        assert_eq!(cache_use(&cached, &request, 149, 1024), CacheUse::Fresh);
        assert_eq!(cache_use(&cached, &request, 150, 1024), CacheUse::Stale);
        assert_eq!(
            cache_use(
                &entry(&[("cache-control", "no-cache, max-age=60")], Vec::new()),
                &request,
                100,
                1024
            ),
            CacheUse::Stale
        );
    }

    #[test]
    fn vary_requires_the_exact_presence_and_value() {
        let mut first = BTreeMap::new();
        first.insert("accept-language".to_owned(), "en".to_owned());
        let cached = cache_entry(
            200,
            &BTreeMap::from([
                ("cache-control".to_owned(), "max-age=60".to_owned()),
                ("vary".to_owned(), "Accept-Language, X-Mode".to_owned()),
            ]),
            b"body".to_vec(),
            &first,
            100,
        )
        .unwrap();
        assert_eq!(cache_use(&cached, &first, 100, 1024), CacheUse::Fresh);

        let mut changed = first.clone();
        changed.insert("accept-language".to_owned(), "fr".to_owned());
        assert_eq!(cache_use(&cached, &changed, 100, 1024), CacheUse::Unusable);
        changed.insert("accept-language".to_owned(), "en".to_owned());
        changed.insert("x-mode".to_owned(), String::new());
        assert_eq!(cache_use(&cached, &changed, 100, 1024), CacheUse::Unusable);
    }

    #[test]
    fn variant_selection_returns_the_matching_representation() {
        let english_headers = BTreeMap::from([("accept-language".to_owned(), "en".to_owned())]);
        let french_headers = BTreeMap::from([("accept-language".to_owned(), "fr".to_owned())]);
        let response_headers = BTreeMap::from([
            ("cache-control".to_owned(), "max-age=60".to_owned()),
            ("vary".to_owned(), "Accept-Language".to_owned()),
        ]);
        let english = cache_entry(
            200,
            &response_headers,
            b"english".to_vec(),
            &english_headers,
            100,
        )
        .unwrap();
        let french = cache_entry(
            200,
            &response_headers,
            b"french".to_vec(),
            &french_headers,
            101,
        )
        .unwrap();

        let (selected, cache_use) =
            select_variant(vec![english, french], &english_headers, 102, 1024).unwrap();
        assert_eq!(selected.body, b"english");
        assert_eq!(cache_use, CacheUse::Fresh);
    }

    #[test]
    fn no_store_wildcard_and_oversized_vary_are_not_cached() {
        let request = BTreeMap::new();
        for vary in [
            "*".to_owned(),
            (0..=MAX_VARY_FIELDS)
                .map(|index| format!("x-{index}"))
                .collect::<Vec<_>>()
                .join(","),
        ] {
            assert!(
                cache_entry(
                    200,
                    &BTreeMap::from([("vary".to_owned(), vary)]),
                    Vec::new(),
                    &request,
                    0
                )
                .is_none()
            );
        }
        assert!(
            cache_entry(
                200,
                &BTreeMap::from([("cache-control".to_owned(), "no-store".to_owned())]),
                Vec::new(),
                &request,
                0
            )
            .is_none()
        );
        assert!(
            cache_entry(
                200,
                &BTreeMap::from([("vary".to_owned(), "X-Large".to_owned())]),
                Vec::new(),
                &BTreeMap::from([("x-large".to_owned(), "x".repeat(MAX_VARY_VALUES_BYTES + 1))]),
                0
            )
            .is_none()
        );
    }
}
