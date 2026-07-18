//! Bounded private HTTP-cache decisions shared by BrowserCore resource loads
//! and page `fetch()`/XHR.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashSet};
use std::time::UNIX_EPOCH;

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
    let request_cache_control = map_header(request_headers, "cache-control");
    if !(200..300).contains(&entry.status)
        || entry.body.len() as u64 > max_body_bytes
        || cache_control_has(&entry.headers, "no-store")
        || request_cache_control.is_some_and(|value| directive(value, "no-store").is_some())
        || !vary_matches(entry, request_headers)
    {
        return CacheUse::Unusable;
    }
    if cache_control_has(&entry.headers, "no-cache")
        || request_cache_control.is_some_and(|value| directive(value, "no-cache").is_some())
        || request_cache_control.is_none()
            && map_header(request_headers, "pragma")
                .is_some_and(|value| directive(value, "no-cache").is_some())
    {
        return CacheUse::Stale;
    }
    let date_value = header(&entry.headers, "date")
        .and_then(http_date_unix)
        .unwrap_or(entry.fetched_unix);
    let apparent_age = entry.fetched_unix.saturating_sub(date_value).max(0) as u64;
    let age_value = header(&entry.headers, "age")
        .map(|value| value.trim().parse::<u64>().unwrap_or(u64::MAX))
        .unwrap_or_default();
    let current_age = apparent_age
        .max(age_value)
        .saturating_add(now_unix.saturating_sub(entry.fetched_unix).max(0) as u64);
    let freshness_lifetime =
        match seconds_directive(header(&entry.headers, "cache-control"), "max-age") {
            SecondsDirective::Value(seconds) => Some(seconds),
            SecondsDirective::Absent => header(&entry.headers, "expires")
                .and_then(http_date_unix)
                .map(|expires| expires.saturating_sub(date_value).max(0) as u64),
            SecondsDirective::Invalid => None,
        };
    let Some(freshness_lifetime) = freshness_lifetime else {
        return CacheUse::Stale;
    };

    let request_max_age = seconds_directive(request_cache_control, "max-age");
    if matches!(request_max_age, SecondsDirective::Invalid)
        || matches!(request_max_age, SecondsDirective::Value(limit) if current_age >= limit)
    {
        return CacheUse::Stale;
    }
    let request_min_fresh = seconds_directive(request_cache_control, "min-fresh");
    if matches!(request_min_fresh, SecondsDirective::Invalid)
        || matches!(request_min_fresh, SecondsDirective::Value(required) if current_age.saturating_add(required) >= freshness_lifetime)
    {
        return CacheUse::Stale;
    }
    let reusable = current_age < freshness_lifetime
        || !cache_control_has(&entry.headers, "must-revalidate")
            && max_stale_allows(request_cache_control, current_age - freshness_lifetime);
    if reusable {
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
    if !(200..300).contains(&status)
        || cache_control_has_map(headers, "no-store")
        || map_header(request_headers, "cache-control")
            .is_some_and(|value| directive(value, "no-store").is_some())
    {
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum SecondsDirective {
    Absent,
    Value(u64),
    Invalid,
}

fn seconds_directive(value: Option<&str>, wanted: &str) -> SecondsDirective {
    let Some(value) = value else {
        return SecondsDirective::Absent;
    };
    let mut found = None;
    for item in value.split(',') {
        let (name, value) = item
            .split_once('=')
            .map_or((item.trim(), None), |(name, value)| {
                (name.trim(), Some(value.trim()))
            });
        if !name.eq_ignore_ascii_case(wanted) {
            continue;
        }
        let Some(seconds) = value
            .map(|value| value.trim_matches('"'))
            .and_then(|value| value.parse::<u64>().ok())
        else {
            return SecondsDirective::Invalid;
        };
        if found.is_some_and(|previous| previous != seconds) {
            return SecondsDirective::Invalid;
        }
        found = Some(seconds);
    }
    found.map_or(SecondsDirective::Absent, SecondsDirective::Value)
}

fn max_stale_allows(cache_control: Option<&str>, staleness: u64) -> bool {
    let Some(cache_control) = cache_control else {
        return false;
    };
    let mut found = false;
    let mut maximum = None;
    for item in cache_control.split(',') {
        let (name, value) = item
            .split_once('=')
            .map_or((item.trim(), None), |(name, value)| {
                (name.trim(), Some(value.trim()))
            });
        if !name.eq_ignore_ascii_case("max-stale") {
            continue;
        }
        found = true;
        let Some(value) = value else {
            return true;
        };
        let Ok(seconds) = value.trim_matches('"').parse::<u64>() else {
            return false;
        };
        maximum = Some(maximum.map_or(seconds, |current: u64| current.min(seconds)));
    }
    found && maximum.is_some_and(|maximum| staleness <= maximum)
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

fn map_header<'a>(headers: &'a BTreeMap<String, String>, wanted: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
        .map(|(_, value)| value.as_str())
}

fn http_date_unix(value: &str) -> Option<i64> {
    let time = httpdate::parse_http_date(value).ok()?;
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).ok(),
        Err(error) => i64::try_from(error.duration().as_secs())
            .ok()
            .and_then(i64::checked_neg),
    }
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
    fn expires_and_age_bound_freshness() {
        let date = "Sun, 06 Nov 1994 08:49:37 GMT";
        let date_unix = http_date_unix(date).unwrap();
        let mut cached = entry(
            &[
                ("date", date),
                ("expires", "Sun, 06 Nov 1994 08:50:37 GMT"),
                ("age", "10"),
            ],
            Vec::new(),
        );
        cached.fetched_unix = date_unix + 5;

        assert_eq!(
            cache_use(&cached, &BTreeMap::new(), date_unix + 54, 1024),
            CacheUse::Fresh
        );
        assert_eq!(
            cache_use(&cached, &BTreeMap::new(), date_unix + 55, 1024),
            CacheUse::Stale
        );

        cached
            .headers
            .push(("cache-control".to_owned(), "max-age=5".to_owned()));
        assert_eq!(
            cache_use(&cached, &BTreeMap::new(), date_unix + 5, 1024),
            CacheUse::Stale
        );
        cached.headers.last_mut().unwrap().1 = "max-age=invalid".to_owned();
        assert_eq!(
            cache_use(&cached, &BTreeMap::new(), date_unix + 5, 1024),
            CacheUse::Stale
        );
    }

    #[test]
    fn request_directives_constrain_reuse_and_storage() {
        let cached = entry(&[("cache-control", "max-age=60")], Vec::new());
        for (value, expected) in [
            ("no-cache", CacheUse::Stale),
            ("max-age=0", CacheUse::Stale),
            ("max-age=10", CacheUse::Stale),
            ("max-age=11", CacheUse::Fresh),
            ("min-fresh=50", CacheUse::Stale),
            ("min-fresh=49", CacheUse::Fresh),
            ("max-age=invalid", CacheUse::Stale),
        ] {
            assert_eq!(
                cache_use(
                    &cached,
                    &BTreeMap::from([("cache-control".to_owned(), value.to_owned())]),
                    110,
                    1024
                ),
                expected,
                "request Cache-Control: {value}"
            );
        }
        assert_eq!(
            cache_use(
                &cached,
                &BTreeMap::from([("pragma".to_owned(), "no-cache".to_owned())]),
                100,
                1024
            ),
            CacheUse::Stale
        );
        let no_store = BTreeMap::from([("cache-control".to_owned(), "no-store".to_owned())]);
        assert_eq!(cache_use(&cached, &no_store, 100, 1024), CacheUse::Unusable);
        assert!(cache_entry(200, &BTreeMap::new(), b"body".to_vec(), &no_store, 100).is_none());
    }

    #[test]
    fn max_stale_does_not_override_revalidation_requirements() {
        let request = BTreeMap::from([("cache-control".to_owned(), "max-stale=5".to_owned())]);
        let cached = entry(&[("cache-control", "max-age=10")], Vec::new());
        assert_eq!(cache_use(&cached, &request, 115, 1024), CacheUse::Fresh);
        assert_eq!(cache_use(&cached, &request, 116, 1024), CacheUse::Stale);
        assert_eq!(
            cache_use(
                &entry(
                    &[("cache-control", "max-age=10, must-revalidate")],
                    Vec::new()
                ),
                &request,
                115,
                1024
            ),
            CacheUse::Stale
        );
        assert_eq!(
            cache_use(
                &entry(&[("cache-control", "max-age=10, no-cache")], Vec::new()),
                &BTreeMap::from([("cache-control".to_owned(), "max-stale".to_owned())]),
                115,
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
